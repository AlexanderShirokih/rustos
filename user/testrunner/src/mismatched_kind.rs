//! В объявленном поле кадра передаётся чужой вид объекта. Контракт объявляет порт, а на
//! сырой границе отправляется `Signal` с правом слать.
//!
//! Метка вида существует только при компиляции, и восстановленный получателем
//! `Cap<kind::Port>` содержит номер записи чужого объекта. Отличает их ядро, и прямой
//! `port_send` этим handle возвращает `WrongType`. Получатель остаётся жив, освобождает
//! запись и обслуживает следующий корректный запрос.

use core::sync::atomic::{AtomicI64, AtomicU32, Ordering::SeqCst};

use ipc::{
    Channel as _, IntoWireValue as _, Listener as _,
    wire::{Header, MESSAGE_INLINE_MAX, MessageBuf},
};
use kernel_objects::{Delivery, OwnedHandle, Port, ReceivedHandle, Signal, kind};
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
        cast fn hand(own capability: ipc::wire::Cap<syscall::kind::Port>);

        /// Запрашивает ответ значением.
        call fn echo(value: u32) -> u32;
    }
}

#[derive(Default)]
struct Probe {
    handed: Option<OwnedHandle<kind::Port>>,
}

impl kind_probe::Service<Delivery> for Probe {
    fn hand(&mut self, _frame: &Delivery, capability: ReceivedHandle<kind::Port>) {
        self.handed = Some(OwnedHandle::from(capability));
    }

    fn echo(&mut self, _frame: &Delivery, value: u32) -> u32 {
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
    let transport = PortTransport::new(port_of(exchange.client.load(SeqCst)));

    // Кадр собран вручную, потому что сгенерированный клиент не пропускает чужой вид в это поле.
    let mut frame = MessageBuf::new();
    let built = frame
        .write_header(&Header::new(kind_probe::hand::ORDINAL))
        .and_then(|()| signal.write_as_field(&mut frame, ipc::wire::FIELD_ID_MIN))
        .and_then(|()| frame.finish());
    if built.is_err() {
        exchange.sent.store(-1, SeqCst);
        return 0;
    }
    let sent = transport.send(frame.as_bytes(), frame.handles(), SEND_TIMEOUT);
    if sent.is_ok() {
        // Ядро забрало запись у отправителя вместе с кадром.
        signal.relinquish();
    }
    exchange.sent.store(i64::from(sent.is_ok()), SeqCst);

    let answered = kind_probe::Client::new(transport)
        .with_wait(CALL_TIMEOUT)
        .echo(PROBE_VALUE);
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

    let transport = PortTransport::new(&service.server);
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    let mut probe = Probe::default();
    let frame = transport
        .recv(&mut bytes, RECV_TIMEOUT)
        .expect("frame delivered");
    kind_probe::dispatch(&mut probe, frame).expect("frame dispatched");
    let owned = probe.handed.take().expect("the capability has an owner");

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
    let frame = transport
        .recv(&mut bytes, RECV_TIMEOUT)
        .expect("next request delivered");
    kind_probe::dispatch(&mut probe, frame).expect("next request answered");

    worker.join(JOIN_TIMEOUT).expect("worker joins");
    kernel_tests::kassert_eq!(exchange.sent.load(SeqCst), 1);
    kernel_tests::kassert_eq!(exchange.answered.load(SeqCst), 1);
}
