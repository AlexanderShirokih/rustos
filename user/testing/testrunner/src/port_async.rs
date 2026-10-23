//! Проверки отложенных операций порта через production-ядро.

use alloc::rc::Rc;
use core::num::NonZeroU64;

use executor::Executor;
use kernel_objects::{Error, Port, Signal};
use runtime::{Timeout, pending_take, signal_wait_one, spawn_owned};
use syscall::{READABLE, Rights, SIGNALED, SyscallError, WaitItem};

use crate::{fixture::JOIN_TIMEOUT, ipc_raw};

#[test_harness::test]
fn port_async_call_runs_both_sides_on_one_executor_thread() {
    let executor = Executor::new().expect("executor");
    let port = Rc::new(Port::create().expect("port"));
    let server = port.clone();
    let reactor = executor.reactor();
    executor.spawn(async move {
        reactor
            .wait(WaitItem::new(server.as_raw(), READABLE))
            .await
            .expect("readable");
        let reply = server.recv(Timeout::POLL).expect("receive").expect("reply");
        let signal = Signal::from(
            ipc_raw::take_cap(0)
                .expect("request capability")
                .assume_kind(),
        );
        signal.wait(SIGNALED, Timeout::POLL).expect("same signal");
        ipc_raw::write_frame(b"answer", &[signal.into_raw().raw()]);
        reply.reply().expect("reply");
    });
    let reactor = executor.reactor();
    executor.block_on(async {
        let signal = Signal::create().expect("signal");
        signal.notify_all().expect("raise");
        let moved = signal.into_raw();
        ipc_raw::write_frame(b"request", &[moved.raw()]);
        let pending = port.call_async().expect("post call");
        test_harness::check_eq!(
            signal_wait_one(moved, SIGNALED, Timeout::POLL),
            i64::from(SyscallError::BadHandle)
        );
        test_harness::check_eq!(
            pending.take(),
            Err(Error::Syscall(SyscallError::ShouldWait))
        );
        test_harness::check!(matches!(
            pending.duplicate(Rights::READ, None),
            Err(Error::Syscall(SyscallError::AccessDenied))
        ));
        reactor
            .wait(WaitItem::new(pending.as_raw(), SIGNALED))
            .await
            .expect("completed");
        pending.take().expect("take");
        let mut body = [0; 6];
        test_harness::check_eq!(ipc_raw::read_frame(&mut body), (6, 1));
        test_harness::check_eq!(&body, b"answer");
        Signal::from(
            ipc_raw::take_cap(0)
                .expect("reply capability")
                .assume_kind(),
        )
        .wait(SIGNALED, Timeout::POLL)
        .expect("reply signal");
        test_harness::check_eq!(
            pending_take(pending.as_raw()),
            i64::from(SyscallError::BadHandle)
        );
    });
}

#[test_harness::test]
fn port_async_send_completes_when_another_thread_receives() {
    let port = Port::create().expect("port");
    let sender = port.duplicate(Rights::WRITE, None).expect("sender");
    let started = Signal::create().expect("started");
    let notify = started.duplicate(Rights::WRITE, None).expect("notify");
    let receiver = spawn_owned(move || {
        notify.notify_all().expect("ready");
        test_harness::check!(port.recv(JOIN_TIMEOUT).expect("receive").is_none());
        let mut body = [0; 4];
        test_harness::check_eq!(ipc_raw::read_frame(&mut body), (4, 0));
        test_harness::check_eq!(&body, b"send");
        0
    })
    .expect("receiver");
    started.wait(SIGNALED, JOIN_TIMEOUT).expect("started");
    ipc_raw::write_frame(b"send", &[]);
    let pending = sender.send_async().expect("post send");
    test_harness::check!(runtime::signal_wait_one(pending.as_raw(), SIGNALED, JOIN_TIMEOUT) > 0);
    pending.take().expect("delivered");
    receiver.join(JOIN_TIMEOUT).expect("join");
}

#[test_harness::test]
fn port_async_staging_failure_preserves_capabilities_and_pending_is_not_transferable() {
    let port = Port::create().expect("port");
    let signal = Signal::create().expect("signal");
    let restricted = signal.duplicate(Rights::READ, None).expect("restricted");
    ipc_raw::write_frame(&[], &[signal.as_raw().raw(), restricted.as_raw().raw()]);
    test_harness::check!(matches!(
        port.send_async(),
        Err(Error::Syscall(SyscallError::AccessDenied))
    ));
    signal.notify_all().expect("first remains");
    restricted
        .wait(SIGNALED, Timeout::POLL)
        .expect("second remains");
    ipc_raw::write_frame(&[], &[]);
    let pending = port.call_async().expect("post");
    ipc_raw::write_frame(&[], &[pending.as_raw().raw()]);
    test_harness::check!(matches!(
        port.send_async(),
        Err(Error::Syscall(SyscallError::AccessDenied))
    ));
    test_harness::check_eq!(
        pending.take(),
        Err(Error::Syscall(SyscallError::ShouldWait))
    );
    drop(pending);
    test_harness::check!(matches!(
        port.recv(Timeout::POLL),
        Err(Error::Syscall(SyscallError::Timeout))
    ));
}

#[test_harness::test]
fn port_async_receiver_departure_returns_caps_and_revokes_the_derived_client() {
    let service = Port::create_service(None).expect("service");
    let signal = Signal::create().expect("signal");
    signal.notify_all().expect("raise");
    ipc_raw::write_frame(&[], &[signal.into_raw().raw()]);
    let pending = service.client.send_async().expect("post");
    drop(service.server);
    test_harness::check_eq!(
        pending.take(),
        Err(Error::Syscall(SyscallError::PeerClosed))
    );
    Signal::from(
        ipc_raw::take_cap(0)
            .expect("returned capability")
            .assume_kind(),
    )
    .wait(SIGNALED, Timeout::POLL)
    .expect("returned signal");
    ipc_raw::write_frame(&[], &[]);
    test_harness::check_eq!(
        service.client.call_async().err(),
        Some(Error::Syscall(SyscallError::Revoked))
    );
}

#[test_harness::test]
fn port_async_cancel_after_receive_keeps_reply_capabilities_at_server() {
    let port = Port::create().expect("port");
    ipc_raw::write_frame(&[], &[]);
    let pending = port.call_async().expect("post");
    let reply = port.recv(Timeout::POLL).expect("receive").expect("reply");
    drop(pending);
    let signal = Signal::create().expect("signal");
    ipc_raw::write_frame(&[], &[signal.as_raw().raw()]);
    test_harness::check_eq!(reply.reply(), Err(Error::Syscall(SyscallError::PeerClosed)));
    signal.notify_all().expect("capability remains");
}

#[test_harness::test]
fn port_async_dropped_reply_signals_and_unclaimed_response_closes_capabilities() {
    let port = Port::create().expect("port");
    ipc_raw::write_frame(&[], &[]);
    let pending = port.call_async().expect("post");
    drop(port.recv(Timeout::POLL).expect("receive").expect("reply"));
    test_harness::check_eq!(
        pending.take(),
        Err(Error::Syscall(SyscallError::PeerClosed))
    );
    ipc_raw::write_frame(&[], &[]);
    let pending = port.call_async().expect("post");
    let reply = port.recv(Timeout::POLL).expect("receive").expect("reply");
    let watched = Port::create().expect("watched");
    let badge = NonZeroU64::new(0xa51).expect("badge");
    let cap = watched
        .duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))
        .expect("badged");
    ipc_raw::write_frame(&[], &[cap.into_raw().raw()]);
    reply.reply().expect("response");
    drop(pending);
    test_harness::check!(matches!(
        watched.recv(Timeout::POLL),
        Err(Error::Syscall(SyscallError::SenderGone))
    ));
}
