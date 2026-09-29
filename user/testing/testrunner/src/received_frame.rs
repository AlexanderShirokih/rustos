//! E2E владения ресурсами доставленного кадра из EL0: главный поток принимает кадры рабочего
//! потока и проверяет судьбу их capability, канала ответа и долга ответа.
//!
//! Каждая доставляемая capability выведена из порта под своим значком
//! ([`Watched`]), и её закрытие видно отказом приёма на
//! этом порту.

use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicI64, Ordering::SeqCst},
};

use ipc::{
    Channel as _, Incoming, Listener, ReceivedResources as _, RecvError, ReplyChannel as _,
    wire::{Header, IpcError, MESSAGE_INLINE_MAX, MessageBuf},
};
use kernel_objects::{Delivery, IssuedHandle, Port, Resource, SessionTransport, Signal, kind};
use kernel_tests::kernel_test;
use runtime::{Mutex, PortTransport, Priority, Timeout, spawn, spawn_owned};
use syscall::{Handle, ProcessName, kind as object_kind};
use userland_loader::load_program;

use crate::{fixture::program, watched_capability::Watched};

const STACK_SIZE: u64 = 0x4000;

const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const RECV_TIMEOUT: Timeout = Timeout::from_secs(5);
const CALL_TIMEOUT: Timeout = Timeout::from_secs(10);
const IDLE_TIMEOUT: Timeout = Timeout::from_millis(200);
const ABANDON_DELAY: Timeout = Timeout::from_secs(1);

const FIRST_BADGE: NonZeroU64 = NonZeroU64::new(0x81).expect("badge is non-zero");
const SECOND_BADGE: NonZeroU64 = NonZeroU64::new(0x82).expect("badge is non-zero");

const PROBE_VALUE: u32 = 0x5A5A;

const TABLE_FULL_ENTRY: &str = "/handle-table-full-fixture/exec";
const TABLE_FULL_NAME: &str = "handle-table-full";

const OUTCOME_ANSWERED: i64 = 1;
const OUTCOME_REFUSED: i64 = 2;
const OUTCOME_TIMEOUT: i64 = 3;

ipc::protocol! {
    /// Контракт пробы владения: кадр с capability, ответ значением и capability, долг ответа.
    pub mod frame_probe {
        name = "FrameProbe";

        /// Передаёт capability односторонним кадром.
        cast fn hand(own capability: ipc::wire::Cap<object_kind::Port>);

        /// Запрашивает ответ значением.
        call(defer) fn echo(value: u32) -> u32;

        /// Запрашивает capability ответом.
        call(defer) fn request() -> own ipc::wire::Cap<object_kind::Port>;

        /// Передаёт долг ответа на `echo` другому держателю.
        cast fn forward(own caller: ipc::Reply<u32>);
    }
}

type EchoReply = frame_probe::echo::Reply<IssuedHandle<object_kind::Reply>>;

type RequestReply = frame_probe::request::Reply<IssuedHandle<object_kind::Reply>>;

struct Exchange {
    port: Handle<kind::Port>,
    capability: Mutex<Option<Port>>,
    outcome: AtomicI64,
}

struct TamperedReply {
    port: Handle<kind::Port>,
    capability: Port,
    scalar: bool,
}

#[derive(Default)]
struct Probe {
    handed: Option<Port>,
    nested: Option<Handle<kind::Port>>,
    nested_answer: Option<Port>,
    vended: Option<Port>,
    keep: bool,
    kept_echo: Option<EchoReply>,
    kept_request: Option<RequestReply>,
    forwarded: Option<bool>,
}

impl frame_probe::Service<Delivery> for Probe {
    fn hand(&mut self, _frame: &Delivery, capability: IssuedHandle<kind::Port>) {
        if let Some(nested) = self.nested {
            let client =
                frame_probe::Client::new(PortTransport::new(nested)).with_wait(CALL_TIMEOUT);
            let answer = client.request().expect("nested call answered");
            self.nested_answer = Some(Port::from(answer));
        }
        self.handed = Some(Port::from(capability));
    }

    fn echo(&mut self, _frame: &Delivery, value: u32, reply: EchoReply) {
        if self.keep {
            self.kept_echo = Some(reply);
        } else {
            let _ = reply.send(value);
        }
    }

    fn request(&mut self, _frame: &Delivery, reply: RequestReply) {
        if self.keep {
            self.kept_request = Some(reply);
        } else {
            let _ = reply.send(self.vended.take().expect("capability of the answer"));
        }
    }

    fn forward(&mut self, _frame: &Delivery, caller: ipc::Reply<u32, IssuedHandle<kind::Reply>>) {
        self.forwarded = Some(caller.send(PROBE_VALUE).is_ok());
    }
}

impl Exchange {
    fn new(port: Handle<kind::Port>) -> Self {
        Self {
            port,
            capability: Mutex::new(None),
            outcome: AtomicI64::new(i64::MIN),
        }
    }

    fn with_capability(port: Handle<kind::Port>, capability: Port) -> Self {
        let exchange = Self::new(port);
        *exchange.capability.lock() = Some(capability);
        exchange
    }

    fn client(&self) -> frame_probe::Client<PortTransport> {
        frame_probe::Client::new(PortTransport::new(self.port)).with_wait(CALL_TIMEOUT)
    }

    fn capability(&self) -> Port {
        self.capability
            .lock()
            .take()
            .expect("capability of the frame")
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

fn serve_one<L: Listener, T>(
    listener: &L,
    wait: Timeout,
    handle: impl FnOnce(Incoming<'_, L::Resources>) -> T,
) -> Result<T, RecvError> {
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    listener.recv(&mut bytes, wait).map(handle)
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
    exchange.store(exchange.client().echo(PROBE_VALUE));
    0
}

extern "C" fn session_echo_worker(arg: usize) -> u32 {
    // SAFETY: см. `exchange_of`.
    let exchange = unsafe { exchange_of(arg) };
    let session = SessionTransport::new(exchange.capability());
    exchange.store(
        frame_probe::Client::new(session)
            .with_wait(CALL_TIMEOUT)
            .echo(PROBE_VALUE),
    );
    0
}

extern "C" fn holder_worker(arg: usize) -> u32 {
    // SAFETY: см. `exchange_of`.
    let exchange = unsafe { exchange_of(arg) };
    let transport = PortTransport::new(exchange.port);
    let mut probe = Probe::default();
    let dispatched = serve_one(&transport, RECV_TIMEOUT, |frame| {
        frame_probe::dispatch(&mut probe, frame)
    });
    exchange.outcome.store(
        i64::from(matches!(dispatched, Ok(Ok(()))) && probe.forwarded == Some(true)),
        SeqCst,
    );
    0
}

extern "C" fn vending_worker(arg: usize) -> u32 {
    // SAFETY: см. `exchange_of`.
    let exchange = unsafe { exchange_of(arg) };
    let transport = PortTransport::new(exchange.port);
    let mut probe = Probe {
        vended: Some(exchange.capability()),
        ..Probe::default()
    };
    let answered = serve_one(&transport, RECV_TIMEOUT, |frame| {
        frame_probe::dispatch(&mut probe, frame)
    });
    exchange
        .outcome
        .store(i64::from(matches!(answered, Ok(Ok(())))), SeqCst);
    0
}

extern "C" fn undelivered_vendor(arg: usize) -> u32 {
    // SAFETY: см. `exchange_of`.
    let exchange = unsafe { exchange_of(arg) };
    let transport = PortTransport::new(exchange.port);
    let mut probe = Probe {
        keep: true,
        ..Probe::default()
    };
    let kept = serve_one(&transport, RECV_TIMEOUT, |frame| {
        frame_probe::dispatch(&mut probe, frame)
    });
    let Some(debt) = probe.kept_request.take() else {
        exchange.outcome.store(0, SeqCst);
        return 0;
    };
    if let Ok(parking) = Port::create() {
        let _ = parking.recv(ABANDON_DELAY);
    }
    let undelivered = debt.send(exchange.capability());

    probe.keep = false;
    let answered = serve_one(&transport, RECV_TIMEOUT, |frame| {
        frame_probe::dispatch(&mut probe, frame)
    });
    exchange.outcome.store(
        i64::from(
            matches!(kept, Ok(Ok(()))) && undelivered.is_err() && matches!(answered, Ok(Ok(()))),
        ),
        SeqCst,
    );
    0
}

fn tampered_reply_worker(exchange: TamperedReply) -> u32 {
    let transport = PortTransport::new(exchange.port);
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    let Incoming {
        bytes: request,
        mut resources,
        ..
    } = transport
        .recv(&mut bytes, RECV_TIMEOUT)
        .expect("request delivered");
    let channel = resources
        .take_reply()
        .expect("reply channel of the request");
    let header = Header::split(request).expect("request header").header;
    let mut reply = MessageBuf::<Delivery>::new();
    reply.write_header(&header).expect("reply header");
    if exchange.scalar {
        reply
            .write_field(1, &PROBE_VALUE.to_le_bytes())
            .expect("scalar reply");
    }
    reply
        .write_handle(2, exchange.capability.into_issued())
        .expect("reply capability");
    reply.finish().expect("reply body");
    channel.send(reply.outgoing()).expect("reply delivered");
    0
}

/// Capability кадра принадлежит параметру обработчика, и завершение кадра её не трогает.
#[kernel_test]
fn a_frame_hands_its_capability_to_the_parameter() {
    let service = Port::create_service(None).expect("port create");
    let watched = Watched::create(FIRST_BADGE);
    let exchange = Exchange::with_capability(service.client.as_raw(), watched.deliver());
    let worker = spawn(
        hand_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn worker");

    let transport = PortTransport::new(&service.server);
    let mut probe = Probe::default();
    serve_one(&transport, RECV_TIMEOUT, |frame| {
        frame_probe::dispatch(&mut probe, frame)
    })
    .expect("frame delivered")
    .expect("frame dispatched");
    let owned = probe.handed.take().expect("the capability has an owner");

    watched.assert_alive();
    drop(owned);
    watched.assert_closed();

    worker.join(JOIN_TIMEOUT).expect("worker joins");
    kernel_tests::kassert_eq!(exchange.outcome.load(SeqCst), 1);
}

/// Вложенный IPC перезаписывает буфер потока, а принятый набор не меняет.
#[kernel_test]
fn a_nested_call_does_not_change_the_received_set() {
    let service = Port::create_service(None).expect("port create");
    let nested = Port::create_service(None).expect("nested port create");
    let handed = Watched::create(FIRST_BADGE);
    let answered = Watched::create(SECOND_BADGE);

    let handing = Exchange::with_capability(service.client.as_raw(), handed.deliver());
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

    let transport = PortTransport::new(&service.server);
    let mut probe = Probe {
        nested: Some(nested.client.as_raw()),
        ..Probe::default()
    };
    serve_one(&transport, RECV_TIMEOUT, |frame| {
        frame_probe::dispatch(&mut probe, frame)
    })
    .expect("frame delivered")
    .expect("frame dispatched");
    let owned = probe.handed.take().expect("the capability has an owner");
    let nested_owned = probe.nested_answer.take().expect("the answer has an owner");

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
    let exchange = Exchange::new(service.client.as_raw());
    let worker = spawn(
        echo_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn worker");

    let transport = PortTransport::new(&service.server);
    serve_one(&transport, RECV_TIMEOUT, |frame| drop(frame)).expect("frame delivered");

    worker.join(JOIN_TIMEOUT).expect("worker joins");
    kernel_tests::kassert_eq!(exchange.outcome.load(SeqCst), OUTCOME_REFUSED);
}

/// Отвеченный кадр reply повторно не закрывает, и его слот занимает следующий объект.
#[kernel_test]
fn an_answered_frame_does_not_close_its_reply_twice() {
    let service = Port::create_service(None).expect("port create");
    let exchange = Exchange::new(service.client.as_raw());
    let worker = spawn(
        echo_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn worker");

    let transport = PortTransport::new(&service.server);
    let mut probe = Probe::default();
    serve_one(&transport, RECV_TIMEOUT, |frame| {
        frame_probe::dispatch(&mut probe, frame)
    })
    .expect("frame delivered")
    .expect("frame answered");
    let signal = Signal::create().expect("signal after the answer");

    kernel_tests::kassert!(signal.notify_all().is_ok());
    worker.join(JOIN_TIMEOUT).expect("worker joins");
    kernel_tests::kassert_eq!(exchange.outcome.load(SeqCst), OUTCOME_ANSWERED);
}

/// Приём, отказавший после переноса capability, оставляет её закрытой. Откат этого переноса
/// держит ядро.
#[kernel_test]
fn a_receive_refused_after_the_transfer_releases_the_delivered_capability() {
    let watched = Watched::create(FIRST_BADGE);
    let service = Port::create_service(None).expect("port create");
    let resource = Resource::self_resource().expect("self metering resource");
    let _fixture = load_program(
        &program(TABLE_FULL_ENTRY),
        ProcessName::new(TABLE_FULL_NAME).expect("fixture process name"),
        &resource,
        service.server,
    )
    .expect("table fixture loads");

    let mut frame = MessageBuf::<Delivery>::new();
    frame
        .write_header(&Header::new(frame_probe::hand::ORDINAL))
        .expect("frame header");
    frame
        .write_handle(1, watched.deliver().into_issued())
        .expect("frame capability");
    frame.finish().expect("frame finished");
    // Таблица фикстуры принимает capability кадра последним свободным слотом, и reply-объекту
    // вызова места уже не остаётся.
    let _ = PortTransport::new(service.client.as_raw()).call(frame.outgoing(), CALL_TIMEOUT);

    watched.assert_closed();
}

/// Ошибка разбора ответа освобождает доставленную capability без участия клиента.
#[kernel_test]
fn a_client_decode_failure_closes_its_reply_capabilities() {
    let service = Port::create_service(None).expect("port create");
    let watched = Watched::create(FIRST_BADGE);
    let exchange = TamperedReply {
        port: service.server.as_raw(),
        capability: watched.deliver(),
        scalar: false,
    };
    let worker = spawn_owned(
        move || tampered_reply_worker(exchange),
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn reply worker");
    let client = frame_probe::Client::new(PortTransport::new(service.client.as_raw()))
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
        capability: watched.deliver(),
        scalar: true,
    };
    let worker = spawn_owned(
        move || tampered_reply_worker(exchange),
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn reply worker");
    let client = frame_probe::Client::new(PortTransport::new(service.client.as_raw()))
        .with_wait(CALL_TIMEOUT);
    let answer = client.echo(PROBE_VALUE).expect("scalar answer");
    kernel_tests::kassert_eq!(answer, PROBE_VALUE);
    watched.assert_closed();
    worker.join(JOIN_TIMEOUT).expect("reply worker joins");
}

/// Прочитанный ответ владеет capability и закрывает её вместе с собой.
#[kernel_test]
fn a_dropped_answer_closes_its_capability() {
    let service = Port::create_service(None).expect("port create");
    let watched = Watched::create(FIRST_BADGE);
    let vending = Exchange::with_capability(service.server.as_raw(), watched.deliver());
    let worker = spawn(
        vending_worker,
        core::ptr::from_ref(&vending) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn vendor");

    let client = frame_probe::Client::new(PortTransport::new(service.client.as_raw()))
        .with_wait(CALL_TIMEOUT);
    let answer = client.request().expect("answered");
    watched.assert_alive();
    drop(answer);

    watched.assert_closed();
    worker.join(JOIN_TIMEOUT).expect("vendor joins");
}

/// Ответ, доставить который некому, закрывает выданную им capability. Получатель остаётся жив
/// и обслуживает следующий корректный запрос.
#[kernel_test]
fn an_undelivered_answer_closes_the_capability_it_vended() {
    let service = Port::create_service(None).expect("port create");
    let watched = Watched::create(FIRST_BADGE);
    let vending = Exchange::with_capability(service.server.as_raw(), watched.deliver());
    let vendor = spawn(
        undelivered_vendor,
        core::ptr::from_ref(&vending) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn vendor");

    let client = frame_probe::Client::new(PortTransport::new(service.client.as_raw()))
        .with_wait(IDLE_TIMEOUT);
    let abandoned = client.request();
    let client = client.with_wait(CALL_TIMEOUT);
    let answered = client.echo(PROBE_VALUE);

    kernel_tests::kassert!(matches!(abandoned, Err(IpcError::Timeout)));
    kernel_tests::kassert_eq!(answered, Ok(PROBE_VALUE));
    watched.assert_closed();
    vendor.join(JOIN_TIMEOUT).expect("vendor joins");
    kernel_tests::kassert_eq!(vending.outcome.load(SeqCst), 1);
}

#[kernel_test]
fn a_kept_debt_is_answered_after_its_frame() {
    let service = Port::create_service(None).expect("port create");
    let exchange = Exchange::new(service.client.as_raw());
    let worker = spawn(
        echo_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn worker");

    let transport = PortTransport::new(&service.server);
    let mut probe = Probe {
        keep: true,
        ..Probe::default()
    };
    serve_one(&transport, RECV_TIMEOUT, |frame| {
        frame_probe::dispatch(&mut probe, frame)
    })
    .expect("frame delivered")
    .expect("frame dispatched");
    let debt = probe.kept_echo.take().expect("the debt is kept");
    let idle = serve_one(&transport, IDLE_TIMEOUT, |frame| drop(frame));

    debt.send(PROBE_VALUE).expect("the kept debt is answered");

    kernel_tests::kassert_eq!(idle, Err(RecvError::Timeout));
    worker.join(JOIN_TIMEOUT).expect("worker joins");
    kernel_tests::kassert_eq!(exchange.outcome.load(SeqCst), OUTCOME_ANSWERED);
}

#[kernel_test]
fn a_forwarded_debt_is_answered_by_its_new_holder() {
    let service = Port::create_service(None).expect("port create");
    let holder_port = Port::create_service(None).expect("holder port create");
    let exchange = Exchange::new(service.client.as_raw());
    let holding = Exchange::new(holder_port.server.as_raw());
    let worker = spawn(
        echo_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn worker");
    let holder = spawn(
        holder_worker,
        core::ptr::from_ref(&holding) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn holder");

    let transport = PortTransport::new(&service.server);
    let mut probe = Probe {
        keep: true,
        ..Probe::default()
    };
    serve_one(&transport, RECV_TIMEOUT, |frame| {
        frame_probe::dispatch(&mut probe, frame)
    })
    .expect("frame delivered")
    .expect("frame dispatched");
    let debt = probe.kept_echo.take().expect("the debt is kept");

    frame_probe::Client::new(PortTransport::new(holder_port.client.as_raw()))
        .with_wait(CALL_TIMEOUT)
        .forward(debt)
        .expect("the debt is forwarded");

    worker.join(JOIN_TIMEOUT).expect("worker joins");
    holder.join(JOIN_TIMEOUT).expect("holder joins");
    kernel_tests::kassert_eq!(exchange.outcome.load(SeqCst), OUTCOME_ANSWERED);
    kernel_tests::kassert_eq!(holding.outcome.load(SeqCst), 1);
}

#[kernel_test]
fn an_unsent_forward_keeps_the_debt_with_its_holder() {
    let service = Port::create_service(None).expect("port create");
    let absent = Port::create_service(None).expect("port without a receiver");
    let first = Exchange::new(service.client.as_raw());
    let second = Exchange::new(service.client.as_raw());
    let first_worker = spawn(
        echo_worker,
        core::ptr::from_ref(&first) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn first worker");

    let transport = PortTransport::new(&service.server);
    let mut probe = Probe {
        keep: true,
        ..Probe::default()
    };
    serve_one(&transport, RECV_TIMEOUT, |frame| {
        frame_probe::dispatch(&mut probe, frame)
    })
    .expect("frame delivered")
    .expect("frame dispatched");
    let debt = probe.kept_echo.take().expect("the debt is kept");

    let refused = frame_probe::Client::new(PortTransport::new(absent.client.as_raw()))
        .with_wait(IDLE_TIMEOUT)
        .forward(debt)
        .expect_err("nobody receives the forward");
    kernel_tests::kassert_eq!(refused.error, IpcError::Timeout);
    refused
        .into_value()
        .expect("the debt returns to its holder")
        .send(PROBE_VALUE)
        .expect("the holder answers its debt");
    first_worker.join(JOIN_TIMEOUT).expect("first worker joins");

    let second_worker = spawn(
        echo_worker,
        core::ptr::from_ref(&second) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn second worker");
    probe.keep = false;
    serve_one(&transport, RECV_TIMEOUT, |frame| {
        frame_probe::dispatch(&mut probe, frame)
    })
    .expect("next frame delivered")
    .expect("next frame answered");

    second_worker
        .join(JOIN_TIMEOUT)
        .expect("second worker joins");
    kernel_tests::kassert_eq!(first.outcome.load(SeqCst), OUTCOME_ANSWERED);
    kernel_tests::kassert_eq!(second.outcome.load(SeqCst), OUTCOME_ANSWERED);
}

#[kernel_test]
fn an_owning_transport_carries_both_sides_of_a_session() {
    let service = Port::create_service(None).expect("port create");
    let client = service.client;
    let exchange = Exchange::with_capability(client.as_raw(), client);
    let worker = spawn(
        session_echo_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn worker");

    let session = SessionTransport::new(service.server);
    let mut probe = Probe::default();
    serve_one(&session, RECV_TIMEOUT, |frame| {
        frame_probe::dispatch(&mut probe, frame)
    })
    .expect("frame delivered")
    .expect("frame answered");

    worker.join(JOIN_TIMEOUT).expect("worker joins");
    kernel_tests::kassert_eq!(exchange.outcome.load(SeqCst), OUTCOME_ANSWERED);
}

#[kernel_test]
fn a_dropped_owning_transport_refuses_its_caller() {
    let service = Port::create_service(None).expect("port create");
    let client = service.client;
    let exchange = Exchange::with_capability(client.as_raw(), client);
    let worker = spawn(
        session_echo_worker,
        core::ptr::from_ref(&exchange) as usize,
        STACK_SIZE,
        Priority::new(1),
    )
    .expect("spawn worker");

    // Порт теряет единственного получателя, и вызов, припаркованный либо следующий, получает
    // отказ.
    drop(SessionTransport::new(service.server));

    worker.join(JOIN_TIMEOUT).expect("worker joins");
    kernel_tests::kassert_eq!(exchange.outcome.load(SeqCst), OUTCOME_REFUSED);
}
