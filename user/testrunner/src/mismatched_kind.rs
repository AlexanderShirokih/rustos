//! В объявленном поле кадра передаётся чужой вид объекта. Контракт объявляет порт, а на
//! сырой границе отправляется `Signal` с правом слать.
//!
//! Метка вида существует только при компиляции, и восстановленный получателем
//! `Cap<kind::Port>` содержит номер записи чужого объекта. Отличает их ядро, и прямой
//! `port_send` этим handle возвращает `WrongType`. Получатель остаётся жив, освобождает
//! запись и обслуживает следующий корректный запрос.

use core::sync::atomic::{AtomicI64, AtomicU32, Ordering::SeqCst};

use ipc::{
    IntoWireHandle as _, Transport as _,
    wire::{Cap, HandleSlots, Header, MESSAGE_INLINE_MAX, MessageBuf},
};
use kernel_objects::{Port, ReceivedFrame, Signal, kind};
use kernel_tests::kernel_test;
use runtime::{PortTransport, Priority, Timeout, handle_close, port_send, spawn};
use syscall::{Handle, Rights, SyscallError};

const STACK_SIZE: u64 = 0x4000;

const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const RECV_TIMEOUT: Timeout = Timeout::from_secs(5);
const CALL_TIMEOUT: Timeout = Timeout::from_secs(10);
const SEND_TIMEOUT: Timeout = Timeout::from_secs(5);

const PROBE_VALUE: u32 = 0x7E7E;

ipc::protocol! {
    /// Контракт, объявляющий поле видом порта.
    pub mod kind_probe {
        name = "KindProbe";

        /// Принимает порт односторонним кадром.
        cast fn hand(capability: Cap<syscall::kind::Port>);

        /// Запрашивает ответ значением.
        call fn echo(value: u32) -> u32;
    }
}

#[derive(Default)]
struct Probe {
    handed: Option<Cap<kind::Port>>,
}

impl kind_probe::Service for Probe {
    fn hand(&mut self, capability: Cap<kind::Port>) {
        self.handed = Some(capability);
    }

    fn echo(&mut self, value: u32) -> u32 {
        value
    }
}

struct Exchange {
    client: AtomicU32,
    signal: AtomicU32,
    sent: AtomicI64,
    answered: AtomicI64,
}

fn port_of(raw: u32) -> Handle<kind::Port> {
    core::num::NonZeroU32::new(raw)
        .map(Handle::from_abi)
        .expect("port handle present")
}

extern "C" fn sending_worker(arg: usize) -> u32 {
    // SAFETY: главный поток держит `Exchange` в своём кадре до `join`.
    let exchange = unsafe { &*(arg as *const Exchange) };
    let signal = Signal::from_handle(
        core::num::NonZeroU32::new(exchange.signal.load(SeqCst))
            .map(Handle::from_abi)
            .expect("signal capability present"),
    );
    let transport = PortTransport::client(port_of(exchange.client.load(SeqCst)))
        .with_send_timeout(SEND_TIMEOUT);

    // Кадр собран вручную, потому что сгенерированный клиент не пропускает чужой вид в это поле.
    let mut frame = MessageBuf::<MESSAGE_INLINE_MAX>::new();
    let mut handles = HandleSlots::new();
    let built = frame
        .write_header(&Header::new(kind_probe::hand::ORDINAL, 0, 0))
        .and_then(|()| signal.write_as_field(&mut frame, ipc::wire::FIELD_ID_MIN, &mut handles))
        .and_then(|()| frame.finish());
    if built.is_err() {
        exchange.sent.store(-1, SeqCst);
        return 0;
    }
    let sent = transport.write_message(frame.as_bytes(), handles.as_slice());
    if sent.is_ok() {
        // Ядро забрало запись у отправителя вместе с кадром.
        signal.relinquish();
    }
    exchange.sent.store(i64::from(sent.is_ok()), SeqCst);

    let answered = kind_probe::Client::new(transport)
        .with_wait(CALL_TIMEOUT)
        .echo(PROBE_VALUE)
        .map(|response| response.value);
    exchange
        .answered
        .store(i64::from(answered == Ok(PROBE_VALUE)), SeqCst);
    0
}

/// Объявленный видом порта параметр довозит `Signal`, и отличает их прямой syscall.
#[kernel_test]
fn a_declared_port_field_carrying_a_signal_is_refused_by_the_kernel() {
    let service = Port::create_service(None).expect("port create");
    let signal = Signal::create().expect("signal create");
    let carried = signal
        .duplicate(Rights::WRITE | Rights::TRANSFER, None)
        .expect("signal capability with the send right");

    let exchange = Exchange {
        client: AtomicU32::new(service.client.as_raw().raw()),
        signal: AtomicU32::new(carried.into_raw().raw()),
        sent: AtomicI64::new(i64::MIN),
        answered: AtomicI64::new(i64::MIN),
    };
    let worker = spawn(
        sending_worker,
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
    kind_probe::dispatch(&mut probe, &transport).expect("frame dispatched");
    let handed = probe.handed.expect("the frame carried a capability");
    let owned = received.claim(handed).expect("the capability has an owner");
    drop(received);

    // Декодирование и claim прошли, потому что вид поля их не проверяет. Операция порта
    // над записью сигнала отказывает в ядре.
    kernel_tests::kassert_eq!(
        port_send(owned.as_raw(), Timeout::POLL),
        SyscallError::WrongType.as_return_value()
    );

    // Уход владельца освобождает запись получателя, и повторное закрытие даёт `BadHandle`.
    let stale = owned.as_raw();
    drop(owned);
    kernel_tests::kassert_eq!(
        handle_close(stale),
        SyscallError::BadHandle.as_return_value()
    );

    // Следующий корректный запрос обслуживает тот же живой получатель.
    transport
        .wait_readable(RECV_TIMEOUT)
        .expect("next request delivered");
    let _next = ReceivedFrame::take(&transport).expect("next request resources");
    kind_probe::dispatch(&mut probe, &transport).expect("next request answered");

    worker.join(JOIN_TIMEOUT).expect("worker joins");
    kernel_tests::kassert_eq!(exchange.sent.load(SeqCst), 1);
    kernel_tests::kassert_eq!(exchange.answered.load(SeqCst), 1);
}
