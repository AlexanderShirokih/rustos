//! E2E живости сторон port из EL0: отказ отправителю на порту без получателей
//! и событие об уходе значка.
//!
//! Роль стороны задаётся правами капы: серверный поток теста держит корневую
//! капу с `READ`, клиент получает минт с `WRITE` и значком. Клиентские потоки
//! обращаются к капе по сырому HandleId: аргумент потока - не хэндл.

use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicI64, AtomicU32, Ordering::SeqCst},
};

use bootstrap_abi::bootstrap;
use kernel_objects::{Error, MemoryRegion, OwnedHandle, Port, Process, Reply, Resource, Result};
use kernel_tests::kernel_test;
use runtime::{
    PortTransport, Priority, Timeout, handle_close, handle_duplicate, ipc_buffer_ptr, port_call,
    port_send, spawn,
};
use syscall::{Handle, Rights, SyscallError};
use userland_image::{component_name, decode};
use userland_loader::load_entry;

use crate::{
    fixture::Fixture,
    fixture_protocol::{OP_WRITE_SLOT, REPLY_OK},
};

const STACK_SIZE: u64 = 0x4000;
const SERVE_TIMEOUT: Timeout = Timeout::from_secs(3);
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const PARK_SETTLE: Timeout = Timeout::from_millis(100);

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

fn client_capability(slot: &AtomicU32) -> Handle {
    Handle::new(slot.load(SeqCst)).expect("client capability id")
}

fn assert_sender_gone(result: Result<Option<Reply>>, badge: u64) {
    kernel_tests::kassert!(matches!(
        result.err(),
        Some(Error::Syscall(SyscallError::SenderGone))
    ));
    kernel_tests::kassert_eq!(received_badge(), badge);
}

fn spawn_image_entry(path: &str, start: OwnedHandle) -> Process {
    let client = bootstrap::Client::new(PortTransport::client(crate::bootstrap_handle()));
    let mut response = client
        .acquire_userland_image()
        .expect("acquire_userland_image call");
    let capability = response.value.expect("vend ok");
    let cap = response.claim(capability).expect("capability owner");
    let region = MemoryRegion::from_handle(OwnedHandle::from(cap));
    let mapping = region.map_full_read_only().expect("map image read-only");
    let image = decode(mapping.as_bytes()).expect("decode acquired image");
    let program = image
        .entry(path)
        .expect("image carries the entry")
        .program()
        .expect("entry record carries a program");
    let resource = Resource::self_resource().expect("self metering resource");
    load_entry(&program, Some(component_name(path)), &resource, start)
        .expect("spawn entry from image")
}

static REFUSED_CLIENT_CAP: AtomicU32 = AtomicU32::new(0);
static REFUSED_HANDSHAKE: AtomicI64 = AtomicI64::new(i64::MIN);
static REFUSED_PARKED_CALL: AtomicI64 = AtomicI64::new(i64::MIN);
static REFUSED_FRESH_CALL: AtomicI64 = AtomicI64::new(i64::MIN);

extern "C" fn refused_client(_arg: usize) -> u32 {
    let client = client_capability(&REFUSED_CLIENT_CAP);
    // Рандеву-подтверждение: сервер роняет свою капу, только узнав, что поток
    // дошёл до порта.
    REFUSED_HANDSHAKE.store(port_send(client, SERVE_TIMEOUT), SeqCst);
    REFUSED_PARKED_CALL.store(port_call(client, SERVE_TIMEOUT), SeqCst);
    REFUSED_FRESH_CALL.store(port_call(client, Timeout::POLL), SeqCst);
    0
}

#[kernel_test]
fn sender_is_refused_when_the_last_receiver_goes() {
    let port = Port::create().expect("port create");
    // Значка у клиентской капы нет: множество получателей считается отдельно от
    // множеств значков.
    let client = port
        .handle()
        .duplicate(Rights::WRITE, None)
        .expect("mint client capability");
    REFUSED_CLIENT_CAP.store(client.as_raw().raw(), SeqCst);
    REFUSED_HANDSHAKE.store(i64::MIN, SeqCst);
    REFUSED_PARKED_CALL.store(i64::MIN, SeqCst);
    REFUSED_FRESH_CALL.store(i64::MIN, SeqCst);

    let worker = spawn(refused_client, 0, STACK_SIZE, Priority::new(1)).expect("spawn client");
    port.recv(SERVE_TIMEOUT).expect("handshake frame");
    // Кадр принят - клиент вышел из send; пауза отдаёт ему процессор, чтобы он
    // припарковался в call до обнуления множества получателей.
    pause(PARK_SETTLE);

    // Корневая капа - единственный держатель READ.
    drop(port);
    worker.join(JOIN_TIMEOUT).expect("client joins");

    kernel_tests::kassert_eq!(REFUSED_HANDSHAKE.load(SeqCst), 0);
    kernel_tests::kassert_eq!(
        SyscallError::from_return(REFUSED_PARKED_CALL.load(SeqCst)),
        Some(SyscallError::PeerClosed)
    );
    // Свежий call идёт по капе, выведенной из закрытой корневой, поэтому отказ
    // приходит отзывом деривации, а не признаком пустого множества.
    kernel_tests::kassert_eq!(
        SyscallError::from_return(REFUSED_FRESH_CALL.load(SeqCst)),
        Some(SyscallError::Revoked)
    );
}

#[kernel_test]
fn badge_departs_when_the_client_process_dies() {
    let port = Port::create().expect("port create");
    // TRANSFER нужен, чтобы капа ушла потомку стартовым хэндлом.
    let client = port
        .handle()
        .duplicate(Rights::WRITE | Rights::TRANSFER, Some(DYING_CLIENT_BADGE))
        .expect("mint client capability");
    let process = spawn_image_entry("/boot/badge-client/program", client);

    port.recv(SERVE_TIMEOUT).expect("client frame");
    kernel_tests::kassert_eq!(received_badge(), DYING_CLIENT_BADGE.get());

    process.join(JOIN_TIMEOUT).expect("client terminates");
    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);

    // Капа осталась в таблице клиента: держателей значка лишила смерть процесса.
    assert_sender_gone(port.recv(SERVE_TIMEOUT), DYING_CLIENT_BADGE.get());
}

static LIVING_CLIENT_BADGED_CAP: AtomicU32 = AtomicU32::new(0);
static LIVING_CLIENT_PLAIN_CAP: AtomicU32 = AtomicU32::new(0);
static LIVING_CLIENT_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);

extern "C" fn living_client(_arg: usize) -> u32 {
    let badged = client_capability(&LIVING_CLIENT_BADGED_CAP);
    let plain = client_capability(&LIVING_CLIENT_PLAIN_CAP);

    let framed = port_send(badged, SERVE_TIMEOUT);
    let closed = handle_close(badged);
    // Кадр без значка после закрытия: им клиент подтверждает, что жив.
    let alive = port_send(plain, SERVE_TIMEOUT);
    let released = handle_close(plain);

    LIVING_CLIENT_RESULT.store(framed | closed | alive | released, SeqCst);
    0
}

#[kernel_test]
fn badge_departs_when_a_living_client_closes_its_capability() {
    let port = Port::create().expect("port create");
    let badged = port
        .handle()
        .duplicate(Rights::WRITE, Some(LIVING_CLIENT_BADGE))
        .expect("mint badged capability");
    let plain = port
        .handle()
        .duplicate(Rights::WRITE, None)
        .expect("mint unbadged capability");
    // Обе капы отданы клиенту: закрывает их он.
    LIVING_CLIENT_BADGED_CAP.store(badged.into_raw().raw(), SeqCst);
    LIVING_CLIENT_PLAIN_CAP.store(plain.into_raw().raw(), SeqCst);
    LIVING_CLIENT_RESULT.store(i64::MIN, SeqCst);

    let worker = spawn(living_client, 0, STACK_SIZE, Priority::new(1)).expect("spawn client");

    port.recv(SERVE_TIMEOUT).expect("badged frame");
    kernel_tests::kassert_eq!(received_badge(), LIVING_CLIENT_BADGE.get());

    assert_sender_gone(port.recv(SERVE_TIMEOUT), LIVING_CLIENT_BADGE.get());

    // Кадр без значка приходит следом: его отправитель был жив, когда значок ушёл.
    port.recv(SERVE_TIMEOUT)
        .expect("frame from the living client");
    kernel_tests::kassert_eq!(received_badge(), 0);

    worker.join(JOIN_TIMEOUT).expect("client joins");
    kernel_tests::kassert_eq!(LIVING_CLIENT_RESULT.load(SeqCst), 0);
}

static SERVED_CLIENT_CAP: AtomicU32 = AtomicU32::new(0);
static SERVED_CLIENT_CLOSED: AtomicU32 = AtomicU32::new(0);
static SERVED_CLIENT_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);

extern "C" fn served_client(_arg: usize) -> u32 {
    let badged = client_capability(&SERVED_CLIENT_CAP);
    let framed = port_send(badged, SERVE_TIMEOUT);
    let closed = handle_close(badged);
    SERVED_CLIENT_RESULT.store(framed | closed, SeqCst);
    SERVED_CLIENT_CLOSED.store(1, SeqCst);
    0
}

#[kernel_test]
fn departed_badge_waits_for_the_busy_server() {
    let port = Port::create().expect("port create");
    let badged = port
        .handle()
        .duplicate(Rights::WRITE, Some(SERVED_CLIENT_BADGE))
        .expect("mint badged capability");
    SERVED_CLIENT_CAP.store(badged.into_raw().raw(), SeqCst);
    SERVED_CLIENT_CLOSED.store(0, SeqCst);
    SERVED_CLIENT_RESULT.store(i64::MIN, SeqCst);

    let worker = spawn(served_client, 0, STACK_SIZE, Priority::new(1)).expect("spawn client");

    port.recv(SERVE_TIMEOUT).expect("client frame");
    kernel_tests::kassert_eq!(received_badge(), SERVED_CLIENT_BADGE.get());

    // Сервер разбирает принятый кадр и в recv не стоит, пока значок уходит.
    while SERVED_CLIENT_CLOSED.load(SeqCst) == 0 {
        core::hint::spin_loop();
    }

    // POLL не паркуется, поэтому событие могло дождаться сервера только в
    // состоянии порта.
    assert_sender_gone(port.recv(Timeout::POLL), SERVED_CLIENT_BADGE.get());

    worker.join(JOIN_TIMEOUT).expect("client joins");
    kernel_tests::kassert_eq!(SERVED_CLIENT_RESULT.load(SeqCst), 0);
}

/// Сервер-потомок узнаёт об уходе клиента без capability на его процесс:
/// закрытие клиентской капы завершает фикстуру, и родителю не нужен terminate.
#[kernel_test]
fn served_child_exits_when_its_client_capability_goes() {
    let fixture = Fixture::spawn();
    // Запрос до закрытия: сессия заведомо жива и обслужена.
    kernel_tests::kassert_eq!(
        fixture.request_status(OP_WRITE_SLOT, SESSION_PROBE, &[]),
        REPLY_OK
    );

    let process = fixture.into_process();
    process.join(JOIN_TIMEOUT).expect("fixture terminates");
    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);
}

/// Уход значка на bootstrap-порту не закрывает kernel-сервер: следующий кадр
/// принимает он же.
#[kernel_test]
fn kernel_server_serves_past_a_departed_badge() {
    let session = OwnedHandle::from_handle(
        handle_duplicate(
            crate::bootstrap_handle(),
            Rights::WRITE.into(),
            BOOTSTRAP_CLIENT_BADGE,
        )
        .expect("mint badged bootstrap capability"),
    );
    session.close().expect("close badged bootstrap capability");

    // Cast встречается с получателем, поэтому его успех означает живого сервера.
    let client = bootstrap::Client::new(PortTransport::client(crate::bootstrap_handle()));
    client
        .log_str(
            "port-liveness",
            "bootstrap server survived a departed badge",
        )
        .expect("log after the badge departure");
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
        .handle()
        .duplicate(Rights::WRITE, Some(FIRST_CLIENT_BADGE))
        .expect("mint first client capability");
    let second = port
        .handle()
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

    // Порядок прихода кадров не определён, поэтому значки сверяются как множество.
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

    // Уходит только первая сессия: событие обязано назвать её значок.
    first.close().expect("close first client capability");
    assert_sender_gone(port.recv(SERVE_TIMEOUT), FIRST_CLIENT_BADGE.get());

    // Вторая сессия была жива и уходит отдельным событием.
    second.close().expect("close second client capability");
    assert_sender_gone(port.recv(SERVE_TIMEOUT), SECOND_CLIENT_BADGE.get());
}
