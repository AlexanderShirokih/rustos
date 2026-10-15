//! E2E приёма порта сессий экспорта [`SessionEvents`] из EL0: отброшенный кадр и уход значка
//! не прерывают приём, опрос пустого порта не ждёт, а capability порта без права приёма
//! заканчивает приём.

use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicI64, Ordering::SeqCst},
};

use component_startup::{Polled, SessionEvent, SessionEvents};
use kernel_objects::{Port, wait_any};
use runtime::{Timeout, port_send, spawn_owned};
use syscall::{READABLE, Rights, SyscallError, WaitItem};

use crate::{fixture::JOIN_TIMEOUT, ipc_raw, resource_exhaustion::exhaust_handle_table};

const SEND_TIMEOUT: Timeout = Timeout::from_secs(3);

const FIRST_BADGE: NonZeroU64 = NonZeroU64::new(0x5e55_0001).expect("badge is non-zero");
const SECOND_BADGE: NonZeroU64 = NonZeroU64::new(0x5e55_0002).expect("badge is non-zero");

static DISCARDED_SEND: AtomicI64 = AtomicI64::new(i64::MIN);
static DELIVERED_SEND: AtomicI64 = AtomicI64::new(i64::MIN);

fn session(port: &Port, badge: NonZeroU64) -> Port {
    port.duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))
        .expect("mint session capability")
}

fn send(session: &Port, caps: &[u32]) -> i64 {
    ipc_raw::write_frame(&[], caps);
    port_send(session.as_raw(), SEND_TIMEOUT)
}

fn frame_badge(event: Option<SessionEvent<'_>>) -> Option<u64> {
    match event {
        Some(SessionEvent::Frame(frame)) => Some(frame.resources.badge),
        _ => None,
    }
}

fn exhausting_sender(session: &Port) -> u32 {
    // Таблица хэндлов общая у отправителя и получателя, и capability кадра получателю
    // некуда принять.
    let (_source, mut fillers) = exhaust_handle_table();
    let carried = fillers.pop().expect("full table contains filler handles");

    DISCARDED_SEND.store(send(session, &[carried.as_raw().raw()]), SeqCst);
    drop(fillers);
    DELIVERED_SEND.store(send(session, &[]), SeqCst);
    0
}

#[test_harness::test]
fn session_events_skip_a_discarded_frame() {
    let port = Port::create().expect("port create");
    let first = session(&port, FIRST_BADGE);
    DISCARDED_SEND.store(i64::MIN, SeqCst);
    DELIVERED_SEND.store(i64::MIN, SeqCst);

    let sender = spawn_owned(move || exhausting_sender(&first)).expect("spawn sender");
    let mut events = SessionEvents::new(&port);

    test_harness::check_eq!(frame_badge(events.recv()), Some(FIRST_BADGE.get()));
    sender.join(JOIN_TIMEOUT).expect("sender joins");
    test_harness::check_eq!(
        DISCARDED_SEND.load(SeqCst),
        SyscallError::OutOfHandles.as_return_value()
    );
    test_harness::check_eq!(DELIVERED_SEND.load(SeqCst), 0);
}

#[test_harness::test]
fn session_events_yield_a_departed_badge() {
    let port = Port::create().expect("port create");
    let first = session(&port, FIRST_BADGE);
    let second = session(&port, SECOND_BADGE);
    DELIVERED_SEND.store(i64::MIN, SeqCst);
    drop(first);

    let sender = spawn_owned(move || {
        DELIVERED_SEND.store(send(&second, &[]), SeqCst);
        0
    })
    .expect("spawn sender");
    let mut events = SessionEvents::new(&port);

    test_harness::check!(matches!(
        events.recv(),
        Some(SessionEvent::Gone(badge)) if badge == FIRST_BADGE.get()
    ));
    test_harness::check_eq!(frame_badge(events.recv()), Some(SECOND_BADGE.get()));
    sender.join(JOIN_TIMEOUT).expect("sender joins");
    test_harness::check_eq!(DELIVERED_SEND.load(SeqCst), 0);
}

#[test_harness::test]
fn session_events_end_without_receive_right() {
    let port = Port::create().expect("port create");
    let unreadable = port
        .duplicate(Rights::WRITE, None)
        .expect("mint capability without READ");

    test_harness::check!(SessionEvents::new(&unreadable).recv().is_none());
}

#[test_harness::test]
fn session_events_poll_an_empty_port() {
    let port = Port::create().expect("port create");
    let _client = session(&port, FIRST_BADGE);

    test_harness::check!(matches!(
        SessionEvents::new(&port).try_recv(),
        Polled::Empty
    ));
}

#[test_harness::test]
fn session_events_poll_a_waiting_frame() {
    let port = Port::create().expect("port create");
    let first = session(&port, FIRST_BADGE);
    DELIVERED_SEND.store(i64::MIN, SeqCst);

    let sender = spawn_owned(move || {
        DELIVERED_SEND.store(send(&first, &[]), SeqCst);
        0
    })
    .expect("spawn sender");
    let items = [WaitItem::new(port.as_raw(), READABLE)];
    test_harness::check!(matches!(wait_any(&items, SEND_TIMEOUT), Ok(0)));

    let mut events = SessionEvents::new(&port);
    test_harness::check!(matches!(
        events.try_recv(),
        Polled::Event(SessionEvent::Frame(frame)) if frame.resources.badge == FIRST_BADGE.get()
    ));
    sender.join(JOIN_TIMEOUT).expect("sender joins");
    test_harness::check_eq!(DELIVERED_SEND.load(SeqCst), 0);
}
