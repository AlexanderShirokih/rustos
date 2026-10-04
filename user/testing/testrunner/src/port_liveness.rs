//! E2E живости сторон port из EL0: отказ отправителю на порту без получателей
//! и событие об уходе значка.
//!
//! Роль стороны задаётся правами капы. Серверный поток теста держит корневую
//! капу с `READ`, клиент получает минт с `WRITE` и значком. Клиентские потоки
//! обращаются к капе по сырому HandleId: аргумент потока - не хэндл.

use core::{
    num::{NonZeroU32, NonZeroU64},
    sync::atomic::{AtomicI64, AtomicU32, Ordering::SeqCst},
};

use bootstrap_abi::{bootstrap, hardware};
use ipc::{
    Listener as _,
    wire::{IpcError, MESSAGE_INLINE_MAX},
};
use kernel_objects::{Error, MemoryRegion, Port, Reply, Result, kind};
use kernel_tests::kernel_test;
use runtime::{
    Instant, PortTransport, Priority, Timeout, handle_close, handle_duplicate, ipc_buffer_ptr,
    port_call, port_send, spawn, spawn_owned,
};
use syscall::{Handle, Rights, SyscallError};

use crate::{
    fixture::{Fixture, spawn_entry},
    fixture_protocol::{OP_WRITE_SLOT, REPLY_OK},
};

const STACK_SIZE: u64 = 0x4000;
const SERVE_TIMEOUT: Timeout = Timeout::from_secs(3);
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const PARK_SETTLE: Timeout = Timeout::from_millis(100);
const DEADLINE_RECV: Timeout = Timeout::from_secs(1);
const SENDER_DELAY: Timeout = Timeout::from_millis(500);

const DYING_CLIENT_BADGE: NonZeroU64 =
    NonZeroU64::new(0x1111_1111_1111_1111).expect("badge is non-zero");
const LIVING_CLIENT_BADGE: NonZeroU64 =
    NonZeroU64::new(0x2222_2222_2222_2222).expect("badge is non-zero");
const SERVED_CLIENT_BADGE: NonZeroU64 =
    NonZeroU64::new(0x3333_3333_3333_3333).expect("badge is non-zero");
const FIRST_CLIENT_BADGE: NonZeroU64 =
    NonZeroU64::new(0x4444_4444_4444_4444).expect("badge is non-zero");
const SECOND_CLIENT_BADGE: NonZeroU64 =
    NonZeroU64::new(0x5555_5555_5555_5555).expect("badge is non-zero");
const BOOTSTRAP_CLIENT_BADGE: u64 = 0x7777_7777_7777_7777;

const SESSION_PROBE: u64 = 0x6666_6666_6666_6666;

const VEND_EXHAUSTION_FIXTURE: &str = "/vend-exhaustion-fixture/exec";

fn received_badge() -> u64 {
    let buffer = ipc_buffer_ptr().expect("thread owns an IPC buffer");
    // SAFETY: ядро замаппило per-thread IPC-буфер user-RW, текущий поток -
    // единственный его владелец; читаем поле в границах буфера.
    unsafe { (*buffer).badge }
}

fn pause(budget: Timeout) {
    let idle = Port::create().expect("port create");
    let waited = idle.recv(budget);
    kernel_tests::kassert!(matches!(waited, Err(Error::Syscall(SyscallError::Timeout))));
}

fn client_capability(slot: &AtomicU32) -> Handle<kind::Port> {
    NonZeroU32::new(slot.load(SeqCst))
        .map(Handle::from_abi)
        .expect("client capability id")
}

fn assert_sender_gone(result: Result<Option<Reply>>, badge: u64) {
    kernel_tests::kassert!(matches!(
        result.err(),
        Some(Error::Syscall(SyscallError::SenderGone))
    ));
    kernel_tests::kassert_eq!(received_badge(), badge);
}

static REFUSED_CLIENT_CAP: AtomicU32 = AtomicU32::new(0);
static REFUSED_HANDSHAKE: AtomicI64 = AtomicI64::new(i64::MIN);
static REFUSED_PARKED_CALL: AtomicI64 = AtomicI64::new(i64::MIN);
static REFUSED_FRESH_CALL: AtomicI64 = AtomicI64::new(i64::MIN);

extern "C" fn refused_client(_arg: usize) -> u32 {
    let client = client_capability(&REFUSED_CLIENT_CAP);
    // Рандеву-подтверждение. Сервер закрывает свою капу, только узнав, что поток
    // дошёл до порта.
    REFUSED_HANDSHAKE.store(port_send(client, SERVE_TIMEOUT), SeqCst);
    REFUSED_PARKED_CALL.store(port_call(client, SERVE_TIMEOUT), SeqCst);
    REFUSED_FRESH_CALL.store(port_call(client, Timeout::POLL), SeqCst);
    0
}

#[kernel_test]
fn sender_is_refused_when_the_last_receiver_goes() {
    let port = Port::create().expect("port create");
    let client = port
        .duplicate(Rights::WRITE, None)
        .expect("mint client capability");
    REFUSED_CLIENT_CAP.store(client.as_raw().raw(), SeqCst);
    REFUSED_HANDSHAKE.store(i64::MIN, SeqCst);
    REFUSED_PARKED_CALL.store(i64::MIN, SeqCst);
    REFUSED_FRESH_CALL.store(i64::MIN, SeqCst);

    let worker = spawn(refused_client, 0, STACK_SIZE, Priority::new(1)).expect("spawn client");
    port.recv(SERVE_TIMEOUT).expect("handshake frame");
    pause(PARK_SETTLE);

    drop(port);
    worker.join(JOIN_TIMEOUT).expect("client joins");

    kernel_tests::kassert_eq!(REFUSED_HANDSHAKE.load(SeqCst), 0);
    kernel_tests::kassert_eq!(
        SyscallError::from_return(REFUSED_PARKED_CALL.load(SeqCst)),
        Some(SyscallError::PeerClosed)
    );
    kernel_tests::kassert_eq!(
        SyscallError::from_return(REFUSED_FRESH_CALL.load(SeqCst)),
        Some(SyscallError::Revoked)
    );
}

#[kernel_test]
fn badge_departs_when_the_client_process_dies() {
    let port = Port::create().expect("port create");
    let client = port
        .duplicate(Rights::WRITE | Rights::TRANSFER, Some(DYING_CLIENT_BADGE))
        .expect("mint client capability");
    let process = spawn_entry("/badge-client/exec", client);

    port.recv(SERVE_TIMEOUT).expect("client frame");
    kernel_tests::kassert_eq!(received_badge(), DYING_CLIENT_BADGE.get());

    process.join(JOIN_TIMEOUT).expect("client terminates");
    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);

    assert_sender_gone(port.recv(SERVE_TIMEOUT), DYING_CLIENT_BADGE.get());
}

static LIVING_CLIENT_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);

fn living_client(badged: Port, plain: Port) -> u32 {
    let framed = port_send(badged.as_raw(), SERVE_TIMEOUT);
    let closed = handle_close(badged.into_issued());
    // Кадр без значка после закрытия. Им клиент подтверждает, что жив.
    let alive = port_send(plain.as_raw(), SERVE_TIMEOUT);
    let released = handle_close(plain.into_issued());

    LIVING_CLIENT_RESULT.store(framed | closed | alive | released, SeqCst);
    0
}

#[kernel_test]
fn badge_departs_when_a_living_client_closes_its_capability() {
    let port = Port::create().expect("port create");
    let badged = port
        .duplicate(Rights::WRITE, Some(LIVING_CLIENT_BADGE))
        .expect("mint badged capability");
    let plain = port
        .duplicate(Rights::WRITE, None)
        .expect("mint unbadged capability");
    LIVING_CLIENT_RESULT.store(i64::MIN, SeqCst);

    let worker = spawn_owned(
        move || living_client(badged, plain),
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn client");

    port.recv(SERVE_TIMEOUT).expect("badged frame");
    kernel_tests::kassert_eq!(received_badge(), LIVING_CLIENT_BADGE.get());

    assert_sender_gone(port.recv(SERVE_TIMEOUT), LIVING_CLIENT_BADGE.get());

    port.recv(SERVE_TIMEOUT)
        .expect("frame from the living client");
    kernel_tests::kassert_eq!(received_badge(), 0);

    worker.join(JOIN_TIMEOUT).expect("client joins");
    kernel_tests::kassert_eq!(LIVING_CLIENT_RESULT.load(SeqCst), 0);
}

static SERVED_CLIENT_CLOSED: AtomicU32 = AtomicU32::new(0);
static SERVED_CLIENT_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);

fn served_client(badged: Port) -> u32 {
    let framed = port_send(badged.as_raw(), SERVE_TIMEOUT);
    let closed = handle_close(badged.into_issued());
    SERVED_CLIENT_RESULT.store(framed | closed, SeqCst);
    SERVED_CLIENT_CLOSED.store(1, SeqCst);
    0
}

#[kernel_test]
fn departed_badge_waits_for_the_busy_server() {
    let port = Port::create().expect("port create");
    let badged = port
        .duplicate(Rights::WRITE, Some(SERVED_CLIENT_BADGE))
        .expect("mint badged capability");
    SERVED_CLIENT_CLOSED.store(0, SeqCst);
    SERVED_CLIENT_RESULT.store(i64::MIN, SeqCst);

    let worker = spawn_owned(move || served_client(badged), STACK_SIZE, Priority::new(1))
        .expect("spawn client");

    port.recv(SERVE_TIMEOUT).expect("client frame");
    kernel_tests::kassert_eq!(received_badge(), SERVED_CLIENT_BADGE.get());

    while SERVED_CLIENT_CLOSED.load(SeqCst) == 0 {
        core::hint::spin_loop();
    }

    assert_sender_gone(port.recv(Timeout::POLL), SERVED_CLIENT_BADGE.get());

    worker.join(JOIN_TIMEOUT).expect("client joins");
    kernel_tests::kassert_eq!(SERVED_CLIENT_RESULT.load(SeqCst), 0);
}

#[kernel_test]
fn served_child_exits_when_its_client_capability_goes() {
    let fixture = Fixture::spawn();
    kernel_tests::kassert_eq!(
        fixture.request_status(OP_WRITE_SLOT, SESSION_PROBE, &[]),
        REPLY_OK
    );

    let process = fixture.into_process();
    process.join(JOIN_TIMEOUT).expect("fixture terminates");
    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);
}

#[kernel_test]
fn kernel_server_serves_past_a_departed_badge() {
    let session = Port::from(
        handle_duplicate(
            crate::bootstrap_handle(),
            Rights::WRITE.into(),
            BOOTSTRAP_CLIENT_BADGE,
        )
        .expect("mint badged bootstrap capability"),
    );
    session.close().expect("close badged bootstrap capability");

    let client = bootstrap::Client::new(PortTransport::new(crate::bootstrap_handle()));
    client
        .log_str(
            "port-liveness",
            "bootstrap server survived a departed badge",
        )
        .expect("log after the badge departure");
}

#[kernel_test]
fn kernel_server_closes_the_capabilities_of_a_discarded_frame() {
    let rider = Port::create().expect("port create");
    let derived = rider
        .duplicate(Rights::WRITE, None)
        .expect("mint derived capability");

    crate::ipc_raw::write_frame(&[0], &[rider.into_raw().raw()]);
    kernel_tests::kassert_eq!(
        SyscallError::from_return(port_call(crate::bootstrap_handle(), SERVE_TIMEOUT)),
        Some(SyscallError::PeerClosed)
    );
    kernel_tests::kassert_eq!(
        SyscallError::from_return(port_send(derived.as_raw(), Timeout::POLL)),
        Some(SyscallError::Revoked)
    );

    crate::hardware_session()
        .client()
        .acquire_irq_control()
        .expect("acquire_irq_control call")
        .expect("vend ok");
}

static FIRST_CLIENT_CAP: AtomicU32 = AtomicU32::new(0);
static SECOND_CLIENT_CAP: AtomicU32 = AtomicU32::new(0);
static FIRST_CLIENT_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);
static SECOND_CLIENT_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);

extern "C" fn first_client(_arg: usize) -> u32 {
    let client = client_capability(&FIRST_CLIENT_CAP);
    FIRST_CLIENT_RESULT.store(port_send(client, SERVE_TIMEOUT), SeqCst);
    0
}

extern "C" fn second_client(_arg: usize) -> u32 {
    let client = client_capability(&SECOND_CLIENT_CAP);
    SECOND_CLIENT_RESULT.store(port_send(client, SERVE_TIMEOUT), SeqCst);
    0
}

#[kernel_test]
fn departed_badge_names_the_session_that_left() {
    let port = Port::create().expect("port create");
    let first = port
        .duplicate(Rights::WRITE, Some(FIRST_CLIENT_BADGE))
        .expect("mint first client capability");
    let second = port
        .duplicate(Rights::WRITE, Some(SECOND_CLIENT_BADGE))
        .expect("mint second client capability");
    FIRST_CLIENT_CAP.store(first.as_raw().raw(), SeqCst);
    SECOND_CLIENT_CAP.store(second.as_raw().raw(), SeqCst);
    FIRST_CLIENT_RESULT.store(i64::MIN, SeqCst);
    SECOND_CLIENT_RESULT.store(i64::MIN, SeqCst);

    let first_worker =
        spawn(first_client, 0, STACK_SIZE, Priority::new(1)).expect("spawn first client");
    let second_worker =
        spawn(second_client, 0, STACK_SIZE, Priority::new(1)).expect("spawn second client");

    let mut seen = [0u64; 2];
    for slot in &mut seen {
        port.recv(SERVE_TIMEOUT).expect("client frame");
        *slot = received_badge();
    }
    kernel_tests::kassert!(seen.contains(&FIRST_CLIENT_BADGE.get()));
    kernel_tests::kassert!(seen.contains(&SECOND_CLIENT_BADGE.get()));

    first_worker.join(JOIN_TIMEOUT).expect("first client joins");
    second_worker
        .join(JOIN_TIMEOUT)
        .expect("second client joins");
    kernel_tests::kassert_eq!(FIRST_CLIENT_RESULT.load(SeqCst), 0);
    kernel_tests::kassert_eq!(SECOND_CLIENT_RESULT.load(SeqCst), 0);

    first.close().expect("close first client capability");
    assert_sender_gone(port.recv(SERVE_TIMEOUT), FIRST_CLIENT_BADGE.get());

    second.close().expect("close second client capability");
    assert_sender_gone(port.recv(SERVE_TIMEOUT), SECOND_CLIENT_BADGE.get());
}

fn stale_handle() -> u32 {
    let port = Port::create().expect("port create");
    let raw = port.as_raw().raw();
    port.close().expect("close port");
    raw
}

static STALE_FRAME_CLIENT_CAP: AtomicU32 = AtomicU32::new(0);
static STALE_FRAME_SEND: AtomicI64 = AtomicI64::new(i64::MIN);
static STALE_FRAME_FOLLOWUP: AtomicI64 = AtomicI64::new(i64::MIN);

extern "C" fn stale_frame_client(_arg: usize) -> u32 {
    let client = client_capability(&STALE_FRAME_CLIENT_CAP);
    crate::ipc_raw::write_frame(&[1], &[stale_handle()]);
    STALE_FRAME_SEND.store(port_send(client, SERVE_TIMEOUT), SeqCst);
    crate::ipc_raw::write_frame(&[2], &[]);
    STALE_FRAME_FOLLOWUP.store(port_send(client, SERVE_TIMEOUT), SeqCst);
    0
}

#[kernel_test]
fn a_frame_with_a_stale_handle_leaves_a_listener_serving() {
    let port = Port::create().expect("port create");
    let client = port
        .duplicate(Rights::WRITE, None)
        .expect("mint client capability");
    STALE_FRAME_CLIENT_CAP.store(client.as_raw().raw(), SeqCst);
    STALE_FRAME_SEND.store(i64::MIN, SeqCst);
    STALE_FRAME_FOLLOWUP.store(i64::MIN, SeqCst);

    let worker = spawn(stale_frame_client, 0, STACK_SIZE, Priority::new(1)).expect("spawn client");
    let server = PortTransport::new(port.as_raw());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    let incoming = server
        .recv(&mut bytes, SERVE_TIMEOUT)
        .expect("frame after the refused one");
    kernel_tests::kassert_eq!(incoming.bytes, &[2u8][..]);

    worker.join(JOIN_TIMEOUT).expect("client joins");
    kernel_tests::kassert_eq!(
        SyscallError::from_return(STALE_FRAME_SEND.load(SeqCst)),
        Some(SyscallError::BadHandle)
    );
    kernel_tests::kassert_eq!(STALE_FRAME_FOLLOWUP.load(SeqCst), 0);
}

static LATE_SENDER_CAP: AtomicU32 = AtomicU32::new(0);
static LATE_SENDER_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);

extern "C" fn late_stale_sender(_arg: usize) -> u32 {
    let client = client_capability(&LATE_SENDER_CAP);
    pause(SENDER_DELAY);
    crate::ipc_raw::write_frame(&[1], &[stale_handle()]);
    LATE_SENDER_RESULT.store(port_send(client, SERVE_TIMEOUT), SeqCst);
    0
}

#[kernel_test]
fn a_failed_transfer_does_not_extend_the_receive_deadline() {
    let port = Port::create().expect("port create");
    let client = port
        .duplicate(Rights::WRITE, None)
        .expect("mint client capability");
    LATE_SENDER_CAP.store(client.as_raw().raw(), SeqCst);
    LATE_SENDER_RESULT.store(i64::MIN, SeqCst);

    let worker = spawn(late_stale_sender, 0, STACK_SIZE, Priority::new(1)).expect("spawn sender");
    let started = Instant::now();
    let received = port.recv(DEADLINE_RECV);
    let elapsed = Instant::now().since(started);

    kernel_tests::kassert!(matches!(
        received,
        Err(Error::Syscall(SyscallError::Timeout))
    ));
    kernel_tests::kassert!(elapsed.raw() < DEADLINE_RECV.raw() + SENDER_DELAY.raw());
    worker.join(JOIN_TIMEOUT).expect("sender joins");
    kernel_tests::kassert_eq!(
        SyscallError::from_return(LATE_SENDER_RESULT.load(SeqCst)),
        Some(SyscallError::BadHandle)
    );
}

#[kernel_test]
fn kernel_server_serves_past_frames_it_refuses() {
    let refused: [(&[u8], &[u32], SyscallError); 3] = [
        (&[0], &[stale_handle()], SyscallError::BadHandle),
        (&[], &[], SyscallError::PeerClosed),
        (&[0xFF; 3], &[], SyscallError::PeerClosed),
    ];
    for (body, caps, error) in refused {
        crate::ipc_raw::write_frame(body, caps);
        kernel_tests::kassert_eq!(
            SyscallError::from_return(port_call(crate::bootstrap_handle(), SERVE_TIMEOUT)),
            Some(error)
        );
    }

    let image = bootstrap::Client::new(PortTransport::new(crate::bootstrap_handle()))
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("vend ok");
    drop(MemoryRegion::from(image));
}

#[kernel_test]
fn kernel_server_closes_what_it_vended_to_a_full_table() {
    let kernel = Port::from(
        handle_duplicate(
            crate::bootstrap_handle(),
            (Rights::WRITE | Rights::TRANSFER).into(),
            0,
        )
        .expect("copy of the kernel port capability"),
    );
    let process = spawn_entry(VEND_EXHAUSTION_FIXTURE, kernel);

    process.join(JOIN_TIMEOUT).expect("fixture exits");
    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);
}

#[kernel_test]
fn a_hardware_session_does_not_receive() {
    let session = crate::hardware_session().into_handle();

    kernel_tests::kassert!(matches!(
        session.recv(Timeout::POLL),
        Err(Error::Syscall(SyscallError::AccessDenied))
    ));
}

#[kernel_test]
fn each_role_refuses_the_operations_of_the_other() {
    let session = crate::hardware_session();
    let as_bootstrap = bootstrap::Client::new(PortTransport::new(session.handle().as_raw()));
    let as_hardware = hardware::Client::new(PortTransport::new(crate::bootstrap_handle()));

    kernel_tests::kassert_eq!(
        as_bootstrap.acquire_userland_image().err(),
        Some(IpcError::PeerClosed)
    );
    kernel_tests::kassert_eq!(
        as_hardware.acquire_irq_control().err(),
        Some(IpcError::PeerClosed)
    );
}
