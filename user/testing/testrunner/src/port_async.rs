//! Проверки отложенных операций порта и асинхронного клиента контракта через production-ядро.

use alloc::rc::Rc;
use core::{cell::Cell, num::NonZeroU64, time::Duration};

use executor::{PortChannel, yield_now};
use ipc::{Listener as _, wire::IpcError};
use kernel_objects::{Delivery, Error, Port, Signal};
use runtime::{PortTransport, Timeout, pending_take, signal_wait_one, sleep, spawn_owned};
use syscall::{READABLE, Rights, SIGNALED, SyscallError, WaitItem, kind};

use crate::{executor::executor, fixture::JOIN_TIMEOUT, ipc_raw};

const REPLY_DELAY: Duration = Duration::from_millis(100);

ipc::protocol! {
    pub mod async_probe {
        name = "AsyncProbe";

        call fn double(value: u32) -> u32;
        cast fn hand(own signal: ipc::wire::Cap<kind::Signal>);
    }
}

struct Doubler;

impl async_probe::Service<Delivery> for Doubler {
    fn double(&mut self, _frame: &Delivery, value: u32) -> u32 {
        value.saturating_mul(2)
    }

    fn hand(&mut self, _frame: &Delivery, _signal: runtime::IssuedHandle<kind::Signal>) {}
}

#[test_harness::test]
fn port_async_client_call_lets_other_tasks_run_until_the_answer() {
    let service = Port::create_service(None).expect("service");
    let server = spawn_owned(move || {
        let listener = PortTransport::new(service.server.as_raw());
        let mut bytes = [0; ipc::wire::MESSAGE_INLINE_MAX];
        let frame = listener.recv(&mut bytes, JOIN_TIMEOUT).expect("call frame");
        sleep(REPLY_DELAY);
        async_probe::dispatch(&mut Doubler, frame).expect("answer");
        0
    })
    .expect("server");

    let executor = executor();
    let ticks = Rc::new(Cell::new(0u64));
    let counted = Rc::clone(&ticks);
    executor.spawn(async move {
        loop {
            counted.set(counted.get().saturating_add(1));
            yield_now().await;
        }
    });

    let client = async_probe::AsyncClient::new(PortChannel::new(
        service.client.as_raw(),
        executor.reactor(),
    ));
    let answer = executor.block_on(client.double(21));

    test_harness::check_eq!(answer, Ok(42));
    test_harness::check!(ticks.get() > 1);
    server.join(JOIN_TIMEOUT).expect("server joins");
}

#[test_harness::test]
fn port_async_client_call_times_out_by_default_and_refuses_the_late_answer() {
    let port = Rc::new(Port::create().expect("port"));
    let executor = executor();
    let reactor = executor.reactor();
    let held = Rc::new(Cell::new(None));
    let (server, holder) = (Rc::clone(&port), Rc::clone(&held));
    executor.spawn(async move {
        reactor
            .wait(WaitItem::new(server.as_raw(), READABLE))
            .await
            .expect("readable");
        holder.set(server.recv(Timeout::POLL).expect("receive"));
    });

    let client = async_probe::AsyncClient::new(PortChannel::new(port.as_raw(), executor.reactor()));
    let answer = executor.block_on(client.double(1));

    test_harness::check_eq!(answer, Err(IpcError::Timeout));

    let reply = held.take().expect("the server holds the reply");
    ipc_raw::write_frame(&[], &[]);
    test_harness::check_eq!(reply.reply(), Err(Error::Syscall(SyscallError::PeerClosed)));
}

#[test_harness::test]
fn port_async_client_cast_returns_the_capability_when_receivers_leave() {
    let service = Port::create_service(None).expect("service");
    let executor = executor();
    let mut server = Some(service.server);
    executor.spawn(async move {
        // Отправка встаёт в очередь порта до ухода получателя.
        yield_now().await;
        drop(server.take());
    });

    let signal = Signal::create().expect("signal");
    signal.notify_all().expect("raise");

    let client = async_probe::AsyncClient::new(PortChannel::new(
        service.client.as_raw(),
        executor.reactor(),
    ));
    let refused = executor
        .block_on(client.hand(signal))
        .expect_err("receivers left");

    test_harness::check_eq!(refused.error, IpcError::PeerClosed);

    let returned = refused.into_value().expect("returned capability");
    test_harness::check!(signal_wait_one(returned.as_raw(), SIGNALED, Timeout::POLL) > 0);
}

#[test_harness::test]
fn port_async_call_runs_both_sides_on_one_executor_thread() {
    let executor = executor();
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
