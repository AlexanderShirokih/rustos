//! E2E `PortTransport`: значок отправителя в ресурсах принятого кадра, срок односторонней
//! отправки и отказ вызова, чей ответ не уместился у вызывающего. Клиент шлёт по кадру через
//! заклеймённую и незаклеймённую копии port-хэндла.

use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicI64, Ordering::SeqCst},
};

use ipc::{
    IntoWireValue, Listener as _,
    wire::{Cap, IpcError, MESSAGE_INLINE_MAX},
};
use kernel_objects::{Delivery, Port};
use runtime::{PortTransport, Timeout, spawn_owned};
use syscall::{Rights, kind};

use crate::{
    answer_exhaustion_protocol::{EXHAUSTED_EXIT_CODE, answer_exhaustion},
    fixture::spawn_entry,
};

const ANSWER_EXHAUSTION_FIXTURE: &str = "/answer-exhaustion-fixture/exec";

const RECV_TIMEOUT: Timeout = Timeout::from_secs(5);
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);

const BADGE: NonZeroU64 = NonZeroU64::new(0x00C0_FFEE).expect("badge is non-zero");

ipc::protocol! {
    pub mod cast_probe {
        name = "CastProbe";

        cast fn hand(value: u64);
    }
}

static CLIENT_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);

fn badge_client(badged: &Port, plain: &Port) -> u32 {
    // При рандеву порядок приёма сервером совпадает с порядком отправок.
    let first = badged.send_bytes(b"badged", Timeout::INFINITE);
    let second = plain.send_bytes(b"plain", Timeout::INFINITE);

    CLIENT_RESULT.store(i64::from(first.is_ok() && second.is_ok()), SeqCst);
    0
}

#[test_harness::test]
fn server_reads_badge_of_receiving_handle() {
    let port = Port::create().expect("port create");
    let badged = port
        .duplicate(Rights::WRITE, Some(BADGE))
        .expect("mint badged copy");
    let plain = port
        .duplicate(Rights::WRITE, None)
        .expect("mint unbadged copy");
    CLIENT_RESULT.store(i64::MIN, SeqCst);

    let client = spawn_owned(move || badge_client(&badged, &plain)).expect("spawn client");

    let server = PortTransport::new(port.as_raw());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    let badged_seen = server
        .recv(&mut bytes, RECV_TIMEOUT)
        .expect("frame from badged copy")
        .resources
        .badge;
    let plain_seen = server
        .recv(&mut bytes, RECV_TIMEOUT)
        .expect("frame from unbadged copy")
        .resources
        .badge;

    test_harness::check_eq!(badged_seen, BADGE.get());
    test_harness::check_eq!(plain_seen, 0);

    client
        .join(JOIN_TIMEOUT)
        .expect("client joins before timeout");
    test_harness::check_eq!(CLIENT_RESULT.load(SeqCst), 1);
}

#[test_harness::test]
fn client_cast_timeout_when_receiver_stays_open_without_recv() {
    let service = Port::create_service(None).expect("port create");
    let client = cast_probe::Client::new(PortTransport::new(service.client.as_raw()))
        .with_wait(Timeout::POLL);

    test_harness::check_eq!(client.hand(7), Err(IpcError::Timeout));
}

#[test_harness::test]
fn client_cast_reports_peer_close_when_receiver_closes_before_recv() {
    let service = Port::create_service(None).expect("port create");
    let client = cast_probe::Client::new(PortTransport::new(service.client.as_raw()))
        .with_wait(Timeout::POLL);
    drop(service.server);

    test_harness::check_eq!(client.hand(7), Err(IpcError::PeerClosed));
}

impl answer_exhaustion::Service<Delivery> for Option<Port> {
    fn request(
        &mut self,
        _frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Cap<kind::Port>> {
        self.take().expect("capability of the answer")
    }
}

#[test_harness::test]
fn client_call_reports_resource_exhaustion_when_the_answer_does_not_fit() {
    let service = Port::create_service(None).expect("port create");
    let process = spawn_entry(ANSWER_EXHAUSTION_FIXTURE, service.client);

    let mut answer = Some(Port::create().expect("capability of the answer"));
    let transport = PortTransport::new(service.server.as_raw());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    let frame = transport
        .recv(&mut bytes, RECV_TIMEOUT)
        .expect("call of the fixture");
    let _ = answer_exhaustion::dispatch(&mut answer, frame);

    process.join(JOIN_TIMEOUT).expect("fixture exits");
    test_harness::check_eq!(process.exit_code().expect("exit code"), EXHAUSTED_EXIT_CODE);
}
