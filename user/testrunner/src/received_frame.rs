//! E2E владения ресурсами доставленного кадра из EL0: главный поток принимает кадры рабочего
//! потока и проверяет судьбу их capability и незавершённого ответа.
//!
//! Каждая доставляемая capability выведена из порта под своим значком
//! ([`Watched`]), и её закрытие видно отказом приёма на
//! этом порту.

use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicI64, AtomicU32, Ordering::SeqCst},
};

use ipc::{
    Transport as _,
    wire::{Cap, IpcError},
};
use kernel_objects::{OwnedHandle, Port, ReceivedFrame, Signal};
use kernel_tests::kernel_test;
use runtime::{PortTransport, Priority, Timeout, spawn};
use syscall::Handle;

use crate::{
    fixture_frames::{launch_entry, send_raw_frame},
    watched_capability::Watched,
};

const STACK_SIZE: u64 = 0x4000;

const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const RECV_TIMEOUT: Timeout = Timeout::from_secs(5);
const CALL_TIMEOUT: Timeout = Timeout::from_secs(10);
const IDLE_TIMEOUT: Timeout = Timeout::from_millis(200);

const FIRST_BADGE: NonZeroU64 = NonZeroU64::new(0x81).expect("badge is non-zero");
const SECOND_BADGE: NonZeroU64 = NonZeroU64::new(0x82).expect("badge is non-zero");

const PROBE_VALUE: u32 = 0x5A5A;

const TABLE_FULL_ENTRY: &str = "/boot/handle-table-full-fixture/program";
const TABLE_FULL_NAME: &str = "handle-table-full";

const OUTCOME_ANSWERED: i64 = 1;
const OUTCOME_REFUSED: i64 = 2;
const OUTCOME_TIMEOUT: i64 = 3;

ipc::protocol! {
    /// Контракт пробы владения: кадр с capability, кадр без ответа и ответ с capability.
    pub mod frame_probe {
        name = "FrameProbe";

        /// Передаёт capability односторонним кадром.
        cast fn hand(capability: Cap);

        /// Запрашивает ответ значением.
        call fn echo(value: u32) -> u32;

        /// Запрашивает capability ответом.
        call fn request() -> Cap;
    }
}

struct Exchange {
    port: Handle,
    capability: AtomicU32,
    outcome: AtomicI64,
}

struct TamperedReply {
    port: Handle,
    capability: u32,
    scalar: bool,
}

#[derive(Default)]
struct Probe {
    handed: Option<Cap>,
    vended: Option<OwnedHandle>,
}

impl frame_probe::Service for Probe {
    fn hand(&mut self, capability: Cap) {
        self.handed = Some(capability);
    }

    fn echo(&mut self, value: u32) -> u32 {
        value
    }

    fn request(&mut self) -> impl ipc::IntoWireHandle {
        self.vended.take().expect("capability of the answer")
    }
}

impl Exchange {
    fn new(port: Handle) -> Self {
        Self {
            port,
            capability: AtomicU32::new(0),
            outcome: AtomicI64::new(i64::MIN),
        }
    }

    fn with_capability(port: Handle, capability: OwnedHandle) -> Self {
        let exchange = Self::new(port);
        exchange
            .capability
            .store(capability.into_raw().raw(), SeqCst);
        exchange
    }

    fn client(&self) -> frame_probe::Client<PortTransport> {
        frame_probe::Client::new(PortTransport::client(self.port)).with_wait(CALL_TIMEOUT)
    }

    fn capability(&self) -> OwnedHandle {
        OwnedHandle::from_raw(self.capability.load(SeqCst)).expect("capability of the frame")
    }

    fn store(&self, answered: Result<u32, IpcError>) {
        let outcome = match answered {
            Ok(_) => OUTCOME_ANSWERED,
            Err(IpcError::Timeout) => OUTCOME_TIMEOUT,
            Err(_) => OUTCOME_REFUSED,
        };
        self.outcome.store(outcome, SeqCst);
    }
}

unsafe fn exchange_of(arg: usize) -> &'static Exchange {
    // SAFETY: `arg` - адрес `Exchange` в кадре главного потока; кадр жив до `join`, который
    // выполняется до выхода из него.
    unsafe { &*(arg as *const Exchange) }
}

extern "C" fn hand_worker(arg: usize) -> u32 {
    // SAFETY: см. `exchange_of`.
    let exchange = unsafe { exchange_of(arg) };
    let handed = exchange.client().hand(exchange.capability());
    exchange.outcome.store(i64::from(handed.is_ok()), SeqCst);
    0
}

extern "C" fn echo_worker(arg: usize) -> u32 {
    // SAFETY: см. `exchange_of`.
    let exchange = unsafe { exchange_of(arg) };
    exchange.store(
        exchange
            .client()
            .echo(PROBE_VALUE)
            .map(|response| response.value),
    );
    0
}

extern "C" fn vending_worker(arg: usize) -> u32 {
    // SAFETY: см. `exchange_of`.
    let exchange = unsafe { exchange_of(arg) };
    let transport = PortTransport::server(exchange.port);
    let mut probe = Probe {
        handed: None,
        vended: Some(exchange.capability()),
    };
    let answered = transport
        .wait_readable(RECV_TIMEOUT)
        .and_then(|()| frame_probe::dispatch(&mut probe, &transport));
    exchange.outcome.store(i64::from(answered.is_ok()), SeqCst);
    0
}

extern "C" fn tampered_reply_worker(arg: usize) -> u32 {
    // SAFETY: главный поток сохраняет аргумент до завершения рабочего потока через join.
    let exchange = unsafe { &*(arg as *const TamperedReply) };
    let capability = OwnedHandle::from_raw(exchange.capability).expect("reply capability");
    let transport = PortTransport::server(exchange.port);
    transport
        .wait_readable(RECV_TIMEOUT)
        .expect("request delivered");
    let _request = ReceivedFrame::take(&transport).expect("request resources");
    let mut bytes = [0; ipc::wire::MESSAGE_INLINE_MAX];
    let mut handles = [0; ipc::wire::MESSAGE_MAX_HANDLES];
    let len = transport
        .read_message(&mut bytes, &mut handles)
        .expect("request read");
    let request = ipc::wire::Header::decode(&bytes[..len.bytes]).expect("request header");
    let mut reply = ipc::wire::MessageBuf::<{ ipc::wire::MESSAGE_INLINE_MAX }>::new();
    reply
        .write_header(&ipc::wire::Header::new(
            request.ordinal,
            request.txid,
            ipc::wire::FLAG_RESPONSE,
        ))
        .expect("reply header");
    if exchange.scalar {
        reply
            .write_field(1, &PROBE_VALUE.to_le_bytes())
            .expect("scalar reply");
    }
    reply.finish().expect("reply body");
    transport
        .write_message(reply.as_bytes(), &[capability.as_raw().raw()])
        .expect("reply delivered");
    let _ = capability.into_raw();
    0
}

/// Кадр отдаёт capability одному владельцу. Повторное извлечение её не выдаёт, а завершение
/// кадра владельца не трогает.
#[kernel_test]
fn a_frame_hands_its_capability_to_one_owner_only() {
    let service = Port::create_service(None).expect("port create");
    let watched = Watched::create(FIRST_BADGE);
    let exchange = Exchange::with_capability(service.client.handle().as_raw(), watched.deliver());
    let worker = spawn(
        hand_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn worker");

    let transport = PortTransport::server(&service.server);
    transport
        .wait_readable(RECV_TIMEOUT)
        .expect("frame delivered");
    let mut received = ReceivedFrame::take(&transport).expect("delivered frame is owned");
    let mut probe = Probe::default();
    frame_probe::dispatch(&mut probe, &transport).expect("frame dispatched");
    let handed = probe.handed.expect("the frame carried a capability");
    let owned = received.claim(handed).expect("the capability has an owner");
    let again = received.claim(handed);
    drop(received);

    kernel_tests::kassert!(again.is_none());
    watched.assert_alive();
    drop(owned);
    watched.assert_closed();

    worker.join(JOIN_TIMEOUT).expect("worker joins");
    kernel_tests::kassert_eq!(exchange.outcome.load(SeqCst), 1);
}

/// Приём, отказавший до доставки, прав на прежнее содержимое IPC-буфера не даёт.
#[kernel_test]
fn a_refused_receive_leaves_the_previous_capability_alone() {
    let service = Port::create_service(None).expect("port create");
    let watched = Watched::create(FIRST_BADGE);
    let exchange = Exchange::with_capability(service.client.handle().as_raw(), watched.deliver());
    let worker = spawn(
        hand_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn worker");

    let transport = PortTransport::server(&service.server);
    transport
        .wait_readable(RECV_TIMEOUT)
        .expect("frame delivered");
    let mut received = ReceivedFrame::take(&transport).expect("delivered frame is owned");
    let mut probe = Probe::default();
    frame_probe::dispatch(&mut probe, &transport).expect("frame dispatched");
    let owned = received
        .claim(probe.handed.expect("the frame carried a capability"))
        .expect("the capability has an owner");
    drop(received);

    // Второй кадр не приходит, и отказ приёма оставляет в буфере capability первого.
    let refused = transport.wait_readable(IDLE_TIMEOUT);
    let after_refusal = ReceivedFrame::take(&transport);

    kernel_tests::kassert_eq!(refused, Err(IpcError::Timeout));
    kernel_tests::kassert!(after_refusal.is_none());
    watched.assert_alive();
    drop(owned);
    watched.assert_closed();

    worker.join(JOIN_TIMEOUT).expect("worker joins");
}

/// Вложенный IPC перезаписывает буфер потока, а принятый набор не меняет.
#[kernel_test]
fn a_nested_call_does_not_change_the_received_set() {
    let service = Port::create_service(None).expect("port create");
    let nested = Port::create_service(None).expect("nested port create");
    let handed = Watched::create(FIRST_BADGE);
    let answered = Watched::create(SECOND_BADGE);

    let handing = Exchange::with_capability(service.client.handle().as_raw(), handed.deliver());
    let vending = Exchange::with_capability(nested.server.as_raw(), answered.deliver());
    let sender = spawn(
        hand_worker,
        core::ptr::from_ref(&handing) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn sender");
    let vendor = spawn(
        vending_worker,
        core::ptr::from_ref(&vending) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn vendor");

    let transport = PortTransport::server(&service.server);
    transport
        .wait_readable(RECV_TIMEOUT)
        .expect("frame delivered");
    let mut received = ReceivedFrame::take(&transport).expect("delivered frame is owned");
    let mut probe = Probe::default();
    frame_probe::dispatch(&mut probe, &transport).expect("frame dispatched");
    let carried = probe.handed.expect("the frame carried a capability");

    // Обработка первого кадра ещё идёт, а вложенный вызов приносит свою capability.
    let nested_client = frame_probe::Client::new(PortTransport::client(nested.client.handle()))
        .with_wait(CALL_TIMEOUT);
    let mut answer = nested_client.request().expect("nested call answered");
    let vended = answer.value;
    let nested_owned = OwnedHandle::from(answer.claim(vended).expect("the answer has an owner"));

    let owned = received
        .claim(carried)
        .expect("the capability has an owner");
    drop(answer);
    drop(received);

    handed.assert_alive();
    answered.assert_alive();
    drop(owned);
    handed.assert_closed();
    drop(nested_owned);
    answered.assert_closed();

    sender.join(JOIN_TIMEOUT).expect("sender joins");
    vendor.join(JOIN_TIMEOUT).expect("vendor joins");
}

/// Кадр, завершённый без ответа, закрывает reply, и вызывающий получает отказ вместо
/// парковки до конца процесса получателя.
#[kernel_test]
fn an_unanswered_frame_refuses_its_caller() {
    let service = Port::create_service(None).expect("port create");
    let exchange = Exchange::new(service.client.handle().as_raw());
    let worker = spawn(
        echo_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn worker");

    let transport = PortTransport::server(&service.server);
    transport
        .wait_readable(RECV_TIMEOUT)
        .expect("frame delivered");
    // Кадр уходит без разбора и без ответа, а его завершение будит вызывающего.
    drop(ReceivedFrame::take(&transport).expect("delivered frame is owned"));

    worker.join(JOIN_TIMEOUT).expect("worker joins");
    kernel_tests::kassert_eq!(exchange.outcome.load(SeqCst), OUTCOME_REFUSED);
}

/// Отвеченный кадр reply повторно не закрывает, и его слот занимает следующий объект.
#[kernel_test]
fn an_answered_frame_does_not_close_its_reply_twice() {
    let service = Port::create_service(None).expect("port create");
    let exchange = Exchange::new(service.client.handle().as_raw());
    let worker = spawn(
        echo_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn worker");

    let transport = PortTransport::server(&service.server);
    transport
        .wait_readable(RECV_TIMEOUT)
        .expect("frame delivered");
    let received = ReceivedFrame::take(&transport).expect("delivered frame is owned");
    let mut probe = Probe::default();
    frame_probe::dispatch(&mut probe, &transport).expect("frame answered");
    // Ответ снял reply с таблицы, и освобождённый слот занимает следующий объект.
    let signal = Signal::create().expect("signal after the answer");
    drop(received);

    kernel_tests::kassert!(signal.notify_all().is_ok());
    worker.join(JOIN_TIMEOUT).expect("worker joins");
    kernel_tests::kassert_eq!(exchange.outcome.load(SeqCst), OUTCOME_ANSWERED);
}

/// Отказавший клиентский вызов доставки не оставляет, и capability прежнего ответа жива.
#[kernel_test]
fn a_refused_call_delivers_nothing_to_its_client() {
    let nested = Port::create_service(None).expect("port create");
    let answered = Watched::create(FIRST_BADGE);
    let vending = Exchange::with_capability(nested.server.as_raw(), answered.deliver());
    let vendor = spawn(
        vending_worker,
        core::ptr::from_ref(&vending) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn vendor");

    let client = frame_probe::Client::new(PortTransport::client(nested.client.handle()))
        .with_wait(CALL_TIMEOUT);
    let mut answer = client.request().expect("call answered");
    let vended = answer.value;
    let owned = OwnedHandle::from(answer.claim(vended).expect("the answer has an owner"));
    drop(answer);
    vendor.join(JOIN_TIMEOUT).expect("vendor joins");

    // Второй вызов встречной стороны не застаёт и не получает ни ответа, ни доставки.
    let client = frame_probe::Client::new(client.into_transport()).with_wait(IDLE_TIMEOUT);
    let refused = client.echo(PROBE_VALUE).map(|response| response.value);
    let transport = client.into_transport();
    let after_refusal = ReceivedFrame::take(&transport);

    kernel_tests::kassert_eq!(refused, Err(IpcError::Timeout));
    kernel_tests::kassert!(after_refusal.is_none());
    answered.assert_alive();
    drop(owned);
    answered.assert_closed();
}

/// Приём, отказавший после переноса capability, оставляет её закрытой. Откат этого переноса
/// держит ядро.
#[kernel_test]
fn a_receive_refused_after_the_transfer_releases_the_delivered_capability() {
    let watched = Watched::create(FIRST_BADGE);

    // Таблица фикстуры принимает capability кадра последним свободным слотом, и reply-объекту
    // вызова места уже не остаётся.
    let _fixture = launch_entry(TABLE_FULL_ENTRY, TABLE_FULL_NAME, |transport| {
        send_raw_frame(&transport, 1, alloc::vec![watched.deliver()])?;
        let _ = transport.wait_readable(CALL_TIMEOUT);
        Ok(())
    });

    watched.assert_closed();
}

/// Ошибка разбора ответа освобождает доставленную capability без участия клиента.
#[kernel_test]
fn a_client_decode_failure_closes_its_reply_capabilities() {
    let service = Port::create_service(None).expect("port create");
    let watched = Watched::create(FIRST_BADGE);
    let exchange = TamperedReply {
        port: service.server.as_raw(),
        capability: watched.deliver().into_raw().raw(),
        scalar: false,
    };
    let worker = spawn(
        tampered_reply_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn reply worker");
    let client = frame_probe::Client::new(PortTransport::client(service.client.handle()))
        .with_wait(CALL_TIMEOUT);
    let answer = client.request();
    kernel_tests::kassert!(matches!(answer, Err(IpcError::MissingField)));
    watched.assert_closed();
    worker.join(JOIN_TIMEOUT).expect("reply worker joins");
}

/// Успешный скалярный ответ не оставляет capability, отсутствующую в его значении.
#[kernel_test]
fn a_scalar_reply_closes_extra_capabilities() {
    let service = Port::create_service(None).expect("port create");
    let watched = Watched::create(FIRST_BADGE);
    let exchange = TamperedReply {
        port: service.server.as_raw(),
        capability: watched.deliver().into_raw().raw(),
        scalar: true,
    };
    let worker = spawn(
        tampered_reply_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn reply worker");
    let client = frame_probe::Client::new(PortTransport::client(service.client.handle()))
        .with_wait(CALL_TIMEOUT);
    let answer = client.echo(PROBE_VALUE).expect("scalar answer").value;
    kernel_tests::kassert_eq!(answer, PROBE_VALUE);
    watched.assert_closed();
    worker.join(JOIN_TIMEOUT).expect("reply worker joins");
}
