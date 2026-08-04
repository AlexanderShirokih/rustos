//! E2E чтения значка отправителя серверной стороной `PortTransport`:
//! клиент шлёт по кадру через заклеймённую и незаклеймённую копии port-хэндла,
//! сервер читает `peer_badge` сразу после приёма.

use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicI64, AtomicU64, Ordering::SeqCst},
};

use ipc::{Transport, wire::IpcError};
use kernel_tests::kernel_test;
use runtime::{OwnedHandle, Port, PortTransport, Priority, Timeout, spawn};
use syscall::Rights;

use crate::startup_fixture_protocol::StartupFixtureClient;

const STACK_SIZE: u64 = 0x4000;
const RECV_TIMEOUT: Timeout = Timeout::from_secs(5);
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);

const BADGE: NonZeroU64 = NonZeroU64::new(0x00C0_FFEE).expect("badge is non-zero");

// Сырые HandleId копий порта, отданных клиенту во владение.
static BADGED_RAW: AtomicU64 = AtomicU64::new(0);
static PLAIN_RAW: AtomicU64 = AtomicU64::new(0);
// Результат клиента: 1 - обе отправки прошли.
static CLIENT_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);

extern "C" fn badge_client(_arg: usize) -> u32 {
    let badged = adopt_port(&BADGED_RAW);
    let plain = adopt_port(&PLAIN_RAW);

    // Рандеву: порядок приёма сервером совпадает с порядком отправок.
    let first = badged.send_bytes(b"badged", Timeout::INFINITE);
    let second = plain.send_bytes(b"plain", Timeout::INFINITE);

    CLIENT_RESULT.store(i64::from(first.is_ok() && second.is_ok()), SeqCst);
    0
}

fn adopt_port(raw: &AtomicU64) -> Port {
    let id = u32::try_from(raw.load(SeqCst)).expect("HandleId fits u32");
    Port::from_handle(OwnedHandle::from_raw(id).expect("minted port copy"))
}

#[kernel_test]
fn server_reads_badge_of_receiving_handle() {
    let port = Port::create().expect("port create");
    // Права сужены до WRITE: клиенту нужен только send. Оригинал незаклеймён,
    // и badge=0 у копии оставляет её незаклеймённой.
    let badged = port
        .handle()
        .duplicate(Rights::WRITE, Some(BADGE))
        .expect("mint badged copy");
    let plain = port
        .handle()
        .duplicate(Rights::WRITE, None)
        .expect("mint unbadged copy");
    BADGED_RAW.store(u64::from(badged.into_raw().raw()), SeqCst);
    PLAIN_RAW.store(u64::from(plain.into_raw().raw()), SeqCst);
    CLIENT_RESULT.store(i64::MIN, SeqCst);

    let client = spawn(badge_client, 0, STACK_SIZE, Priority::new(1)).expect("spawn client");

    // Значки снимаются в локальные переменные до assert'ов: провал kassert
    // логирует строку, а это port_send, перезаписывающий буфер.
    let server = PortTransport::server(port.handle());
    server
        .wait_readable(RECV_TIMEOUT)
        .expect("frame from badged copy");
    let badged_seen = server.peer_badge();
    server
        .wait_readable(RECV_TIMEOUT)
        .expect("frame from unbadged copy");
    let plain_seen = server.peer_badge();

    kernel_tests::kassert_eq!(badged_seen, BADGE.get());
    kernel_tests::kassert_eq!(plain_seen, 0);

    client
        .join(JOIN_TIMEOUT)
        .expect("client joins before timeout");
    kernel_tests::kassert_eq!(CLIENT_RESULT.load(SeqCst), 1);
}

#[kernel_test]
fn client_cast_timeout_when_receiver_stays_open_without_recv() {
    let service = Port::create_service(None).expect("port create");
    let default = PortTransport::client(service.client.handle());
    kernel_tests::kassert_eq!(default.send_timeout(), Timeout::INFINITE);

    let bounded = PortTransport::client(service.client.handle()).with_send_timeout(Timeout::POLL);
    kernel_tests::kassert_eq!(bounded.send_timeout(), Timeout::POLL);
    let client = StartupFixtureClient::new(bounded);

    kernel_tests::kassert_eq!(client.start(7), Err(IpcError::Timeout));
}

#[kernel_test]
fn client_cast_reports_peer_close_when_receiver_closes_before_recv() {
    let service = Port::create_service(None).expect("port create");
    let bounded = PortTransport::client(service.client.handle()).with_send_timeout(Timeout::POLL);
    drop(service.server);

    let client = StartupFixtureClient::new(bounded);
    kernel_tests::kassert_eq!(client.start(7), Err(IpcError::PeerClosed));
}
