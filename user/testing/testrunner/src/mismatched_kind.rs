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
use kernel_objects::{Delivery, IssuedHandle, OwnedHandle, Port, Signal, kind};
use kernel_tests::kernel_test;
use runtime::{PortTransport, Priority, Timeout, port_send, signal_set, spawn_owned};
use syscall::{Handle, Rights, SIGNALED, SyscallError, WakeCount};

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
    fn hand(&mut self, _frame: &Delivery, capability: IssuedHandle<kind::Port>) {
        self.handed = Some(OwnedHandle::from(capability));
    }

    fn echo(&mut self, _frame: &Delivery, value: u32) -> u32 {
        value
    }
}

struct Exchange {
    client: AtomicU32,
    sent: AtomicI64,
    answered: AtomicI64,
}

fn port_of(raw: u32) -> Handle<kind::Port> {
    core::num::NonZeroU32::new(raw)
        .map(Handle::from_abi)
        .expect("port handle present")
}

fn sending_worker(arg: usize, signal: Signal) -> u32 {
    // SAFETY: главный поток держит `Exchange` в своём кадре до `join`.
    let exchange = unsafe { &*(arg as *const Exchange) };
    let transport = PortTransport::new(port_of(exchange.client.load(SeqCst)));

    // Кадр собран вручную, потому что сгенерированный клиент не пропускает чужой вид в это поле.
    let mut frame = MessageBuf::new();
    let built = frame
        .write_header(&Header::new(kind_probe::hand::ORDINAL))
        .and_then(|()| signal.write_into(&mut frame, ipc::wire::FIELD_ID_MIN))
        .and_then(|()| frame.finish());
    if built.is_err() {
        exchange.sent.store(-1, SeqCst);
        return 0;
    }
    let sent = transport.send(frame.outgoing(), SEND_TIMEOUT);
    exchange.sent.store(i64::from(sent.is_ok()), SeqCst);

    let answered = kind_probe::Client::new(transport)
        .with_wait(CALL_TIMEOUT)
        .echo(PROBE_VALUE);
    exchange
        .answered
        .store(i64::from(answered == Ok(PROBE_VALUE)), SeqCst);
    0
}

#[kernel_test]
fn a_declared_port_field_carrying_a_signal_is_refused_by_the_kernel() {
    let service = Port::create_service(None).expect("port create");
    let signal = Signal::create().expect("signal create");
    let carried = signal
        .duplicate(Rights::WRITE | Rights::TRANSFER, None)
        .expect("signal capability with the send right");

    let exchange = Exchange {
        client: AtomicU32::new(service.client.as_raw().raw()),
        sent: AtomicI64::new(i64::MIN),
        answered: AtomicI64::new(i64::MIN),
    };
    let exchange_addr = core::ptr::from_ref(&exchange) as usize;
    let worker = spawn_owned(
        move || sending_worker(exchange_addr, carried),
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

    kernel_tests::kassert_eq!(
        port_send(owned.as_raw(), Timeout::POLL),
        SyscallError::WrongType.as_return_value()
    );

    let stale = Handle::<kind::Signal>::from_abi(owned.as_raw().as_nonzero());
    drop(owned);
    kernel_tests::kassert_eq!(
        signal_set(stale, SIGNALED, 0, WakeCount::None),
        SyscallError::BadHandle.as_return_value()
    );

    let frame = transport
        .recv(&mut bytes, RECV_TIMEOUT)
        .expect("next request delivered");
    kind_probe::dispatch(&mut probe, frame).expect("next request answered");

    worker.join(JOIN_TIMEOUT).expect("worker joins");
    kernel_tests::kassert_eq!(exchange.sent.load(SeqCst), 1);
    kernel_tests::kassert_eq!(exchange.answered.load(SeqCst), 1);
}
