//! `Port` - объект без очереди сообщений в ядре. Внутри он держит очередь ждущих потоков:
//! либо отправителей, либо получателей - но никогда оба одновременно.
//!
//! Транспорт сообщения - per-thread IPC-буфер. Тело копируется
//! напрямую между IPC-буферами двух потоков через линейное физическое отображение.
//! Хендлы переносятся кросс-процессно через handle-таблицы (см. [`ipc_buffer_xfer`](super::ipc_buffer_xfer)).
//!
//! Парковка/пробуждение используют [`ParkWaker`](super::wait): CAS на
//! `state` (REGISTERED -> SIGNALED/CANCELED) выбирает ровно одного победителя
//! в гонке match/cancel.

use alloc::{collections::VecDeque, sync::Arc};
use core::sync::atomic::{AtomicU32, Ordering};

use collections::{LockCell, MutexCell};
use memory::{memory_mapper::MemoryMapper, virtual_address::VirtualAddress};

use super::{
    errors::IpcError,
    handle_table::HandleTable,
    ipc_buffer_xfer::transfer_rendezvous,
    reply::{Reply, ReplySlot},
    runtime::{KernelRuntime, ParkState},
    wait::{CancelTarget, ParkWaker},
};

/// Роль стороны, стоящей в очереди отправителей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendKind {
    /// `send`: после доставки отправитель разблокируется немедленно.
    Send,
    /// `call`: вызывающая сторона остаётся блокированной до `reply`.
    Call,
}

/// Исход операции, прочитанный разбуженной стороной из [`OutcomeSlot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RendezvousOutcome {
    /// Встреча состоялась, сообщение доставлено.
    Delivered,
    /// Port/Reply уничтожен или последний хендл закрыт.
    PeerGone,
    /// Перенос caps или копирование тела провалились.
    TransferFailed,
}

/// Kernel-резидентный IPC-буфер, разделяемый kernel-потоком и
/// рандеву-переносом. Тот же `#[repr(C)]`-layout, что и user-буфер.
pub type KernelIpcBuffer = Arc<MutexCell<syscall::IpcBuffer>>;

/// Способ доступа к телу IPC-буфера транспорта.
#[derive(Clone)]
pub(crate) enum BufferAccess {
    /// User-буфер: mapper его AS и VA буфера.
    User {
        mapper: Arc<dyn MemoryMapper + Send + Sync>,
        ipc_buffer_va: VirtualAddress,
    },
    /// Kernel-резидентный буфер за разделяемой ячейкой.
    Kernel { buffer: KernelIpcBuffer },
}

/// Транспортный контекст потока для кросс-AS рандеву: доступ к телу его
/// IPC-буфера ([`BufferAccess`]) и его handle-таблице. Снимок берётся до
/// того, как поток уйдёт спать.
///
/// `badge` - значок port-хендла, через который отправитель инициировал send/call.
/// После успешного рандеву значок отправителя записывается в `IpcBuffer.badge` получателя.
#[derive(Clone)]
pub struct ThreadTransport {
    pub(crate) access: BufferAccess,
    pub(crate) handle_table: Arc<MutexCell<HandleTable>>,
    pub(crate) badge: u64,
}

impl ThreadTransport {
    /// Транспорт user-потока: тело в буфере, замапленном в его user-AS.
    pub fn new(
        mapper: Arc<dyn MemoryMapper + Send + Sync>,
        ipc_buffer_va: VirtualAddress,
        handle_table: Arc<MutexCell<HandleTable>>,
    ) -> Self {
        Self {
            access: BufferAccess::User {
                mapper,
                ipc_buffer_va,
            },
            handle_table,
            badge: 0,
        }
    }

    pub fn new_kernel(buffer: KernelIpcBuffer, handle_table: Arc<MutexCell<HandleTable>>) -> Self {
        Self {
            access: BufferAccess::Kernel { buffer },
            handle_table,
            badge: 0,
        }
    }

    pub fn with_badge(mut self, badge: u64) -> Self {
        self.badge = badge;
        self
    }
}

/// Атомарный слот исхода рандеву.
pub(crate) struct OutcomeSlot {
    state: AtomicU32,
}

impl OutcomeSlot {
    pub(crate) const PENDING: u32 = 0;
    /// `call`: запрос доставлен серверу, вызывающая сторона ждёт reply.
    pub(crate) const AWAITING_REPLY: u32 = 1;
    /// Перенос ответа в процессе: вызывающая сторона обязан дождаться
    /// терминального исхода, не уходя по тайм-ауту.
    pub(crate) const DELIVERING: u32 = 2;
    pub(crate) const DELIVERED: u32 = 3;
    pub(crate) const PEER_GONE: u32 = 4;
    pub(crate) const TRANSFER_FAILED: u32 = 5;
    /// Вызывающая сторона ушла по тайм-ауту до reply.
    pub(crate) const TIMEDOUT: u32 = 6;

    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU32::new(Self::PENDING),
        })
    }

    pub(crate) fn set(&self, outcome: RendezvousOutcome) {
        let v = match outcome {
            RendezvousOutcome::Delivered => Self::DELIVERED,
            RendezvousOutcome::PeerGone => Self::PEER_GONE,
            RendezvousOutcome::TransferFailed => Self::TRANSFER_FAILED,
        };
        self.state.store(v, Ordering::Release);
    }

    /// Терминальный исход; `None` для нетерминальных состояний
    /// (`PENDING`/`AWAITING_REPLY`/`DELIVERING`/`TIMEDOUT`).
    pub fn get(&self) -> Option<RendezvousOutcome> {
        match self.state.load(Ordering::Acquire) {
            Self::DELIVERED => Some(RendezvousOutcome::Delivered),
            Self::PEER_GONE => Some(RendezvousOutcome::PeerGone),
            Self::TRANSFER_FAILED => Some(RendezvousOutcome::TransferFailed),
            _ => None,
        }
    }

    /// Сырое состояние слота.
    pub(crate) fn raw(&self) -> u32 {
        self.state.load(Ordering::Acquire)
    }

    /// `call`-матчер: запрос доставлен, переводим вызывающую сторону в ожидание reply.
    pub(crate) fn set_awaiting_reply(&self) {
        self.state.store(Self::AWAITING_REPLY, Ordering::Release);
    }

    /// Сервер-reply коммитит доставку ответа: `AWAITING_REPLY -> DELIVERING`.
    /// `true`, если выиграл гонку с тайм-аутом вызывателя.
    pub(crate) fn try_begin_reply(&self) -> bool {
        self.state
            .compare_exchange(
                Self::AWAITING_REPLY,
                Self::DELIVERING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Вызывающая сторона уходит по тайм-ауту ожидания reply: `AWAITING_REPLY ->
    /// TIMEDOUT`. `true`, если выиграл гонку с reply.
    pub(crate) fn try_timeout(&self) -> bool {
        self.state
            .compare_exchange(
                Self::AWAITING_REPLY,
                Self::TIMEDOUT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

// K - роль стороны: SendKind в очереди отправителей и () в очереди получателей.
struct Waiter<K> {
    kind: K,
    transport: ThreadTransport,
    waker: Arc<ParkWaker>,
    outcome: Arc<OutcomeSlot>,
    reply_slot: Arc<ReplySlot>,
}

type SenderWaiter = Waiter<SendKind>;
type ReceiverWaiter = Waiter<()>;

/// Очередь ожидания: либо отправители/вызыватели, либо получатели, но не оба.
enum Queue {
    Empty,
    Senders(VecDeque<SenderWaiter>),
    Receivers(VecDeque<ReceiverWaiter>),
}

/// Port - объект, реализующий синхронный обмен сообщениями.
pub struct Port {
    inner: MutexCell<Queue>,
}

/// Контекст парковки текущего потока: waker и слот исхода.
pub(crate) struct ParkCtx {
    waker: Arc<ParkWaker>,
    outcome: Arc<OutcomeSlot>,
}

impl ParkCtx {
    /// Вызывать только вне queue-лока. `current_wait_token` берёт scheduler-лок, а
    /// обратный порядок захвата занят терминацией потока (`PortCancel`).
    pub(crate) fn for_current(runtime: &Arc<dyn KernelRuntime>) -> Self {
        let token = runtime.current_wait_token();
        Self {
            waker: Arc::new(ParkWaker::new(runtime.clone(), token)),
            outcome: OutcomeSlot::new(),
        }
    }
}

pub(crate) enum SendAction {
    /// Текущий поток обязан заблокироваться на `waker.state()`.
    Park {
        waker: Arc<ParkWaker>,
        outcome: Arc<OutcomeSlot>,
    },
    /// Сообщение доставлено немедленно, без блокировки текущего потока.
    Done,
    /// Ошибка (перенос caps/копирование тела провалились).
    Failed(IpcError),
}

/// Варианта `Done` нет: вызывающая сторона всегда паркуется до reply.
pub(crate) enum CallAction {
    Park {
        waker: Arc<ParkWaker>,
        outcome: Arc<OutcomeSlot>,
    },
    Failed(IpcError),
}

pub(crate) enum RecvAction {
    Park {
        waker: Arc<ParkWaker>,
        outcome: Arc<OutcomeSlot>,
        reply_slot: Arc<ReplySlot>,
    },
    /// Сообщение получено немедленно. `reply` непуст, если встречным был `call`.
    Done {
        reply: Option<Arc<Reply>>,
    },
    Failed(IpcError),
}

impl Port {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MutexCell::new(Queue::Empty),
        })
    }

    /// `send` текущего потока: доставляет ждущему получателю либо паркуется.
    ///
    /// Матч и постановка в очередь - разные захваты queue-лока: между ними берётся
    /// wait-token. Появившийся в это окно получатель возвращает waiter обратно, и цикл
    /// повторяет матч.
    pub(crate) fn send(
        &self,
        mut sender: ThreadTransport,
        runtime: &Arc<dyn KernelRuntime>,
    ) -> SendAction {
        loop {
            if let Some(receiver) = self.inner.with_lock(pop_receiver) {
                return deliver_send(&sender, &receiver);
            }

            let ctx = ParkCtx::for_current(runtime);
            let waiter = Waiter {
                kind: SendKind::Send,
                transport: sender,
                waker: ctx.waker.clone(),
                outcome: ctx.outcome.clone(),
                reply_slot: ReplySlot::new(),
            };
            match self.inner.with_lock(|q| try_push_sender(q, waiter)) {
                Ok(()) => {
                    return SendAction::Park {
                        waker: ctx.waker,
                        outcome: ctx.outcome,
                    };
                }
                Err(returned) => sender = returned.transport,
            }
        }
    }

    /// `call` текущего потока с готовым контекстом парковки. Ретрай - как в [`Self::send`].
    pub(crate) fn call(&self, mut caller: ThreadTransport, ctx: ParkCtx) -> CallAction {
        loop {
            if let Some(receiver) = self.inner.with_lock(pop_receiver) {
                return deliver_call(caller, &receiver, ctx);
            }

            let waiter = Waiter {
                kind: SendKind::Call,
                transport: caller,
                waker: ctx.waker.clone(),
                outcome: ctx.outcome.clone(),
                reply_slot: ReplySlot::new(),
            };
            match self.inner.with_lock(|q| try_push_sender(q, waiter)) {
                Ok(()) => {
                    return CallAction::Park {
                        waker: ctx.waker,
                        outcome: ctx.outcome,
                    };
                }
                Err(returned) => caller = returned.transport,
            }
        }
    }

    /// `recv` текущего потока: забирает сообщение у ждущего отправителя либо паркуется.
    /// Ретрай - как в [`Self::send`].
    pub(crate) fn recv(
        &self,
        mut receiver: ThreadTransport,
        runtime: &Arc<dyn KernelRuntime>,
    ) -> RecvAction {
        loop {
            if let Some(sender) = self.inner.with_lock(pop_sender) {
                return deliver_recv(sender, &receiver);
            }

            let ctx = ParkCtx::for_current(runtime);
            let reply_slot = ReplySlot::new();
            let waiter = Waiter {
                kind: (),
                transport: receiver,
                waker: ctx.waker.clone(),
                outcome: ctx.outcome.clone(),
                reply_slot: reply_slot.clone(),
            };
            match self.inner.with_lock(|q| try_push_receiver(q, waiter)) {
                Ok(()) => {
                    return RecvAction::Park {
                        waker: ctx.waker,
                        outcome: ctx.outcome,
                        reply_slot,
                    };
                }
                Err(returned) => receiver = returned.transport,
            }
        }
    }

    /// Будит всех ждущих с исходом `PeerGone`.
    pub fn cancel_all(&self) {
        let drained = self
            .inner
            .with_lock(|q| core::mem::replace(q, Queue::Empty));
        match drained {
            Queue::Empty => {}
            Queue::Senders(ss) => cancel_each(ss),
            Queue::Receivers(rs) => cancel_each(rs),
        }
    }

    /// Снимает с очереди waiter по идентичности `waker`.
    /// Возвращает `true`, если он был найден и удалён.
    pub(crate) fn remove_waiter(&self, waker: &Arc<ParkWaker>) -> bool {
        self.inner.with_lock(|q| {
            let removed = match q {
                Queue::Empty => false,
                Queue::Senders(dq) => remove_by_waker(dq, waker),
                Queue::Receivers(dq) => remove_by_waker(dq, waker),
            };
            collapse_if_empty(q);
            removed
        })
    }

    #[cfg(test)]
    pub(crate) fn queue_is_empty(&self) -> bool {
        self.inner.with_lock(|q| matches!(q, Queue::Empty))
    }
}

fn deliver_send(sender: &ThreadTransport, receiver: &ReceiverWaiter) -> SendAction {
    match transfer_rendezvous(sender, &receiver.transport) {
        Ok(()) => {
            receiver.outcome.set(RendezvousOutcome::Delivered);
            receiver.waker.signal_match();
            SendAction::Done
        }
        Err(e) => {
            receiver.outcome.set(RendezvousOutcome::TransferFailed);
            receiver.waker.signal_match();
            SendAction::Failed(e)
        }
    }
}

fn deliver_call(caller: ThreadTransport, receiver: &ReceiverWaiter, ctx: ParkCtx) -> CallAction {
    match transfer_rendezvous(&caller, &receiver.transport) {
        Ok(()) => {
            ctx.outcome.set_awaiting_reply();
            let reply = Reply::new(caller, ctx.waker.clone(), ctx.outcome.clone());
            receiver.reply_slot.install(reply);
            receiver.outcome.set(RendezvousOutcome::Delivered);
            receiver.waker.signal_match();
            CallAction::Park {
                waker: ctx.waker,
                outcome: ctx.outcome,
            }
        }
        Err(e) => {
            receiver.outcome.set(RendezvousOutcome::TransferFailed);
            receiver.waker.signal_match();
            CallAction::Failed(e)
        }
    }
}

fn deliver_recv(sender: SenderWaiter, receiver: &ThreadTransport) -> RecvAction {
    match transfer_rendezvous(&sender.transport, receiver) {
        Ok(()) => match sender.kind {
            SendKind::Send => {
                sender.outcome.set(RendezvousOutcome::Delivered);
                sender.waker.signal_match();
                RecvAction::Done { reply: None }
            }
            SendKind::Call => {
                sender.outcome.set_awaiting_reply();
                let reply = Reply::new(sender.transport, sender.waker, sender.outcome);
                RecvAction::Done { reply: Some(reply) }
            }
        },
        Err(e) => {
            sender.outcome.set(RendezvousOutcome::TransferFailed);
            sender.waker.signal_match();
            RecvAction::Failed(e)
        }
    }
}

fn cancel_each<K>(waiters: VecDeque<Waiter<K>>) {
    for w in waiters {
        w.outcome.set(RendezvousOutcome::PeerGone);
        w.waker.cancel();
    }
}

fn remove_by_waker<K>(dq: &mut VecDeque<Waiter<K>>, waker: &Arc<ParkWaker>) -> bool {
    match dq.iter().position(|w| Arc::ptr_eq(&w.waker, waker)) {
        Some(pos) => {
            dq.remove(pos);
            true
        }
        None => false,
    }
}

fn outcome_to_result(outcome: &Arc<OutcomeSlot>) -> Result<(), IpcError> {
    match outcome.get() {
        Some(RendezvousOutcome::Delivered) => Ok(()),
        Some(RendezvousOutcome::PeerGone) => Err(IpcError::PeerClosed),
        Some(RendezvousOutcome::TransferFailed) => Err(IpcError::BufferTooSmall),
        // Парковка завершилась без публикации исхода - трактуем как Canceled.
        None => Err(IpcError::Canceled),
    }
}

fn park_with_timeout(
    runtime: &Arc<dyn KernelRuntime>,
    waker: &Arc<ParkWaker>,
    timeout_ns: Option<u64>,
) {
    if timeout_ns != Some(0) {
        runtime.block_current_until(waker.state(), timeout_ns);
    }
}

fn wait_resolved(runtime: &Arc<dyn KernelRuntime>, waker: &Arc<ParkWaker>) {
    while waker.state().load(Ordering::Acquire) == ParkState::REGISTERED {
        runtime.block_current_until(waker.state(), None);
    }
}

fn resolve_send_recv(
    port: &Arc<Port>,
    runtime: &Arc<dyn KernelRuntime>,
    waker: &Arc<ParkWaker>,
    outcome: &Arc<OutcomeSlot>,
) -> Result<(), IpcError> {
    if port.remove_waiter(waker) {
        waker.claim_timeout();
        return Err(IpcError::Timeout);
    }
    wait_resolved(runtime, waker);
    outcome_to_result(outcome)
}

pub fn port_send(
    port: &Arc<Port>,
    sender: ThreadTransport,
    runtime: &Arc<dyn KernelRuntime>,
    timeout_ns: Option<u64>,
) -> Result<(), IpcError> {
    match port.send(sender, runtime) {
        SendAction::Done => Ok(()),
        SendAction::Failed(e) => Err(e),
        SendAction::Park { waker, outcome } => {
            arm_port_cancel(port, runtime, &waker, &outcome);
            park_with_timeout(runtime, &waker, timeout_ns);
            let result = resolve_send_recv(port, runtime, &waker, &outcome);
            runtime.clear_blocked_cancel();
            result
        }
    }
}

/// Реализует call, блокирующий поток до reply. Ответ оказывается в IPC-буфере вызывателя.
/// `timeout_ns` ограничивает всю операцию (ожидание получателя + ожидание
/// reply);
/// семантика значений - как у [`port_send`].
/// На истечении - [`IpcError::Timeout`].
pub fn port_call(
    port: &Arc<Port>,
    caller: ThreadTransport,
    runtime: &Arc<dyn KernelRuntime>,
    timeout_ns: Option<u64>,
) -> Result<(), IpcError> {
    // вызывающая сторона всегда паркуется до reply - предсоздаём park-ctx, чтобы
    // Reply мог на него ссылаться даже на синхронном матче.
    match port.call(caller, ParkCtx::for_current(runtime)) {
        CallAction::Failed(e) => Err(e),
        CallAction::Park { waker, outcome } => {
            arm_port_cancel(port, runtime, &waker, &outcome);
            park_with_timeout(runtime, &waker, timeout_ns);
            let result = resolve_call(port, runtime, &waker, &outcome, timeout_ns);
            runtime.clear_blocked_cancel();
            result
        }
    }
}

/// Resolve для `call`.
///
/// Фаза 1 (вызывающая сторона ещё в очереди отправителей): снятие себя с очереди под queue-локом.
/// Фаза 2 (запрос уже забран получателем, ждём reply): арбитраж тайм-аута идёт по [`OutcomeSlot`].
/// Пока получатель/сервер переносит данные (`PENDING`/`DELIVERING`), мы дожидаемся завершения переноса -
/// так наш IPC-буфер не будет переиспользован под активным чтением/записью переноса.
fn resolve_call(
    port: &Arc<Port>,
    runtime: &Arc<dyn KernelRuntime>,
    waker: &Arc<ParkWaker>,
    outcome: &Arc<OutcomeSlot>,
    timeout_ns: Option<u64>,
) -> Result<(), IpcError> {
    if port.remove_waiter(waker) {
        waker.claim_timeout();
        return Err(IpcError::Timeout);
    }
    loop {
        match outcome.raw() {
            // Получатель читает наш буфер либо сервер переносит ответ - паркуемся.
            // Партнёр опубликует исход и/или разбудит нас (reply -> signal_match),
            // иначе проснёмся по перевзведённому дедлайну и переоценим состояние.
            OutcomeSlot::PENDING | OutcomeSlot::DELIVERING => {
                runtime.block_current_until(waker.state(), timeout_ns);
            }
            OutcomeSlot::AWAITING_REPLY => {
                if outcome.try_timeout() {
                    waker.claim_timeout();
                    return Err(IpcError::Timeout);
                }
                // Иначе сервер закоммитил reply (DELIVERING) - дочитываем.
            }
            OutcomeSlot::TIMEDOUT => return Err(IpcError::Timeout),
            _ => return outcome_to_result(outcome),
        }
    }
}

/// `recv`: блокирующая.
/// Возвращает `Some(reply)`, если встречным был `call`.
/// `timeout_ns` - как у [`port_send`]; на истечении - [`IpcError::Timeout`].
pub fn port_recv(
    port: &Arc<Port>,
    receiver: ThreadTransport,
    runtime: &Arc<dyn KernelRuntime>,
    timeout_ns: Option<u64>,
) -> Result<Option<Arc<Reply>>, IpcError> {
    match port.recv(receiver, runtime) {
        RecvAction::Done { reply } => Ok(reply),
        RecvAction::Failed(e) => Err(e),
        RecvAction::Park {
            waker,
            outcome,
            reply_slot,
        } => {
            arm_port_cancel(port, runtime, &waker, &outcome);
            park_with_timeout(runtime, &waker, timeout_ns);
            let resolved = resolve_send_recv(port, runtime, &waker, &outcome);
            runtime.clear_blocked_cancel();
            resolved?;

            Ok(reply_slot.take())
        }
    }
}

/// Схлопывает очередь в `Empty`, когда её дек опустел: единственный источник
/// инварианта "пустой дек -> состояние `Empty`".
fn collapse_if_empty(q: &mut Queue) {
    let drained = match q {
        Queue::Empty => false,
        Queue::Senders(dq) => dq.is_empty(),
        Queue::Receivers(dq) => dq.is_empty(),
    };
    if drained {
        *q = Queue::Empty;
    }
}

fn pop_receiver(q: &mut Queue) -> Option<ReceiverWaiter> {
    let waiter = match q {
        Queue::Receivers(rs) => rs.pop_front(),
        Queue::Empty | Queue::Senders(_) => None,
    };
    collapse_if_empty(q);
    waiter
}

fn pop_sender(q: &mut Queue) -> Option<SenderWaiter> {
    let waiter = match q {
        Queue::Senders(ss) => ss.pop_front(),
        Queue::Empty | Queue::Receivers(_) => None,
    };
    collapse_if_empty(q);
    waiter
}

// Err - в очереди уже стоят получатели. Вызывающий обязан повторить матч.
fn try_push_sender(q: &mut Queue, waiter: SenderWaiter) -> Result<(), SenderWaiter> {
    match q {
        Queue::Senders(ss) => {
            ss.push_back(waiter);
            Ok(())
        }
        Queue::Empty => {
            let mut dq = VecDeque::new();
            dq.push_back(waiter);
            *q = Queue::Senders(dq);
            Ok(())
        }
        Queue::Receivers(_) => Err(waiter),
    }
}

fn try_push_receiver(q: &mut Queue, waiter: ReceiverWaiter) -> Result<(), ReceiverWaiter> {
    match q {
        Queue::Receivers(rs) => {
            rs.push_back(waiter);
            Ok(())
        }
        Queue::Empty => {
            let mut dq = VecDeque::new();
            dq.push_back(waiter);
            *q = Queue::Receivers(dq);
            Ok(())
        }
        Queue::Senders(_) => Err(waiter),
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

struct PortCancel {
    port: Arc<Port>,
    waker: Arc<ParkWaker>,
    outcome: Arc<OutcomeSlot>,
}

impl CancelTarget for PortCancel {
    fn cancel(&self) {
        self.port.remove_waiter(&self.waker);
        self.outcome.try_timeout();
    }
}

fn arm_port_cancel(
    port: &Arc<Port>,
    runtime: &Arc<dyn KernelRuntime>,
    waker: &Arc<ParkWaker>,
    outcome: &Arc<OutcomeSlot>,
) {
    let cancel: Arc<dyn CancelTarget> = Arc::new(PortCancel {
        port: port.clone(),
        waker: waker.clone(),
        outcome: outcome.clone(),
    });
    runtime.set_blocked_cancel(cancel);
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::{
        num::NonZeroU64,
        sync::atomic::{AtomicU32, Ordering},
    };

    use collections::MutexCell;
    use memory::virtual_address::VirtualAddress;
    use syscall::encode_tag;

    use super::{
        super::{
            CancelTarget, HandleTable,
            ipc_buffer_xfer::test_mapper::PageMapper,
            runtime::{KernelRuntime, WaitToken},
        },
        *,
    };

    const BASE_A: usize = 0x4000_0000;
    const BASE_B: usize = 0x5000_0000;

    /// Stub-runtime: block_current_until - no-op, unblock считается.
    struct StubRuntime {
        unblocks: AtomicU32,
    }

    impl StubRuntime {
        fn arc() -> Arc<dyn KernelRuntime> {
            Arc::new(Self {
                unblocks: AtomicU32::new(0),
            })
        }
    }

    impl KernelRuntime for StubRuntime {
        fn current_wait_token(&self) -> WaitToken {
            WaitToken::new(NonZeroU64::new(1).unwrap())
        }
        fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
            None
        }
        fn exit_current_thread(&self, _c: i32) -> ! {
            unreachable!()
        }
        fn block_current_until(&self, _f: &AtomicU32, _t: Option<u64>) {}
        fn unblock(&self, _t: WaitToken) {
            self.unblocks.fetch_add(1, Ordering::AcqRel);
        }
        fn set_blocked_cancel(&self, _cancel: Arc<dyn CancelTarget>) {}
        fn clear_blocked_cancel(&self) {}
    }

    fn transport(base: usize) -> ThreadTransport {
        use memory::memory_mapper::MemoryMapper;

        let mapper = PageMapper::new(base);
        let table = Arc::new(MutexCell::new(HandleTable::new()));

        mapper
            .copy_user_out(VirtualAddress::new(base), &encode_tag(3, 0).to_le_bytes())
            .unwrap();
        mapper
            .copy_user_out(VirtualAddress::new(base + 24), b"abc")
            .unwrap();
        ThreadTransport::new(mapper, VirtualAddress::new(base), table)
    }

    #[test]
    fn recv_with_no_sender_parks_as_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let action = ep.recv(transport(BASE_B), &rt);
        assert!(matches!(action, RecvAction::Park { .. }));
        assert!(!ep.queue_is_empty());
    }

    #[test]
    fn send_matches_parked_receiver_and_delivers() {
        let rt = StubRuntime::arc();
        let ep = Port::new();

        // Получатель паркуется.
        let recv_action = ep.recv(transport(BASE_B), &rt);
        let RecvAction::Park {
            outcome: recv_outcome,
            reply_slot: recv_reply_slot,
            ..
        } = recv_action
        else {
            panic!("recv must park");
        };
        assert!(recv_outcome.get().is_none());

        // Отправитель встречает получателя: матч, доставка, queue пуст.
        let send_action = ep.send(transport(BASE_A), &rt);
        assert!(matches!(send_action, SendAction::Done));
        assert_eq!(recv_outcome.get(), Some(RendezvousOutcome::Delivered));
        assert!(ep.queue_is_empty());
        // send -> recv: reply отсутствует.
        assert!(recv_reply_slot.take().is_none());
    }

    #[test]
    fn send_with_no_receiver_parks_as_sender() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let action = ep.send(transport(BASE_A), &rt);
        assert!(matches!(action, SendAction::Park { .. }));
        assert!(!ep.queue_is_empty());
    }

    #[test]
    fn recv_matches_parked_sender() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let send_action = ep.send(transport(BASE_A), &rt);
        let SendAction::Park {
            outcome: send_outcome,
            ..
        } = send_action
        else {
            panic!("send must park");
        };

        let recv_action = ep.recv(transport(BASE_B), &rt);
        assert!(matches!(recv_action, RecvAction::Done { reply: None }));
        assert_eq!(send_outcome.get(), Some(RendezvousOutcome::Delivered));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn call_matched_by_recv_yields_reply() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        // вызывающая сторона паркуется (call всегда паркуется).
        let ctx = ParkCtx::for_current(&rt);
        let outcome = ctx.outcome.clone();
        let call_action = ep.call(transport(BASE_A), ctx);
        assert!(matches!(call_action, CallAction::Park { .. }));

        // Получатель смэтчил call -> получает Reply, вызывающая сторона остаётся
        // заблокированным (outcome ещё pending).
        let recv_action = ep.recv(transport(BASE_B), &rt);
        let RecvAction::Done { reply: Some(reply) } = recv_action else {
            panic!("recv must return a reply for call");
        };
        assert!(outcome.get().is_none(), "caller stays blocked until reply");

        // reply доставляет ответ вызывателю -> outcome Delivered.
        let server = transport(BASE_B);
        reply.reply(&server).expect("reply ok");
        assert_eq!(outcome.get(), Some(RendezvousOutcome::Delivered));
        // one-shot: повторный reply отвергнут.
        assert_eq!(reply.reply(&server).unwrap_err(), IpcError::BadHandle);
    }

    #[test]
    fn cancel_all_wakes_waiters_peer_gone() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let action = ep.recv(transport(BASE_B), &rt);
        let RecvAction::Park { outcome, .. } = action else {
            panic!("recv must park");
        };
        ep.cancel_all();
        assert_eq!(outcome.get(), Some(RendezvousOutcome::PeerGone));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn drop_port_cancels_parked_waiter() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let action = ep.recv(transport(BASE_B), &rt);
        let RecvAction::Park { outcome, .. } = action else {
            panic!("recv must park");
        };
        drop(ep);
        assert_eq!(outcome.get(), Some(RendezvousOutcome::PeerGone));
    }

    #[test]
    fn port_cancel_removes_waiter_on_termination() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let RecvAction::Park { waker, outcome, .. } = ep.recv(transport(BASE_B), &rt) else {
            panic!("recv must park");
        };
        assert!(!ep.queue_is_empty());

        // Терминация заблокированного потока: scheduler зовёт PortCancel,
        // снимающий Waiter с очереди - матчер позже не доставит в мёртвый поток.
        let cancel = PortCancel {
            port: ep.clone(),
            waker: waker.clone(),
            outcome: outcome.clone(),
        };
        CancelTarget::cancel(&cancel);
        assert!(ep.queue_is_empty());

        // Идемпотентность: повторный cancel - no-op.
        CancelTarget::cancel(&cancel);
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn port_cancel_phase2_timeouts_caller_and_rejects_late_reply() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        // вызывающая сторона паркуется (call всегда паркуется до reply).
        let ctx = ParkCtx::for_current(&rt);
        let (waker, outcome) = (ctx.waker.clone(), ctx.outcome.clone());
        let call_action = ep.call(transport(BASE_A), ctx);
        assert!(matches!(call_action, CallAction::Park { .. }));

        // Получатель забрал запрос -> вызывающая сторона в фазе 2 (AWAITING_REPLY); его
        // Waiter уже не в очереди, поэтому remove_waiter в cancel - no-op.
        let RecvAction::Done { reply: Some(reply) } = ep.recv(transport(BASE_B), &rt) else {
            panic!("recv must return a reply for call");
        };
        assert_eq!(outcome.raw(), OutcomeSlot::AWAITING_REPLY);

        // Терминация: PortCancel переводит слот в TIMEDOUT (фаза 2) - так
        // поздний reply сервера не пишет в освобождаемый IPC-буфер.
        let cancel = PortCancel {
            port: ep.clone(),
            waker: waker.clone(),
            outcome: outcome.clone(),
        };
        CancelTarget::cancel(&cancel);
        assert_eq!(outcome.raw(), OutcomeSlot::TIMEDOUT);

        // Поздний reply отвергнут - сервер получает PeerClosed.
        let server = transport(BASE_B);
        assert_eq!(reply.reply(&server).unwrap_err(), IpcError::PeerClosed);

        // Идемпотентность: повторный cancel - no-op.
        CancelTarget::cancel(&cancel);
        assert_eq!(outcome.raw(), OutcomeSlot::TIMEDOUT);
    }

    #[test]
    fn remove_waiter_is_arbiter_and_idempotent() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let RecvAction::Park { waker, .. } = ep.recv(transport(BASE_B), &rt) else {
            panic!("recv must park");
        };
        assert!(!ep.queue_is_empty());
        // Снятие найденного waiter'а -> true, очередь пустеет.
        assert!(ep.remove_waiter(&waker));
        assert!(ep.queue_is_empty());
        // Идемпотентность: повторное снятие -> false.
        assert!(!ep.remove_waiter(&waker));
    }

    #[test]
    fn port_send_poll_without_receiver_is_timeout() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        // poll (Some(0)) без получателя не блокируется - сразу Timeout, и
        // отправитель снят с очереди.
        assert_eq!(
            port_send(&ep, transport(BASE_A), &rt, Some(0)),
            Err(IpcError::Timeout)
        );
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn port_recv_poll_without_sender_is_timeout() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        assert_eq!(
            port_recv(&ep, transport(BASE_B), &rt, Some(0)).map(|_| ()),
            Err(IpcError::Timeout)
        );
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn port_send_poll_matches_parked_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        // Получатель уже припаркован - poll-send встречает его немедленно.
        let RecvAction::Park {
            outcome: recv_outcome,
            ..
        } = ep.recv(transport(BASE_B), &rt)
        else {
            panic!("recv must park");
        };
        assert_eq!(port_send(&ep, transport(BASE_A), &rt, Some(0)), Ok(()));
        assert_eq!(recv_outcome.get(), Some(RendezvousOutcome::Delivered));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn outcome_slot_reply_timeout_arbitration() {
        let a = OutcomeSlot::new();
        a.set_awaiting_reply();
        assert!(a.try_timeout());
        assert!(!a.try_begin_reply()); // reply отклонён - вызывающая сторона ушёл

        let b = OutcomeSlot::new();
        b.set_awaiting_reply();
        assert!(b.try_begin_reply());
        assert!(!b.try_timeout()); // тайм-аут проиграл - reply коммитнут
    }

    #[test]
    fn call_caller_timeout_before_reply_rejects_server_reply() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        // вызывающая сторона паркуется (call всегда паркуется до reply).
        let ctx = ParkCtx::for_current(&rt);
        let (waker, outcome) = (ctx.waker.clone(), ctx.outcome.clone());
        let call_action = ep.call(transport(BASE_A), ctx);
        assert!(matches!(call_action, CallAction::Park { .. }));

        let RecvAction::Done { reply: Some(reply) } = ep.recv(transport(BASE_B), &rt) else {
            panic!("recv must return a reply for call");
        };
        assert_eq!(outcome.raw(), OutcomeSlot::AWAITING_REPLY);

        assert!(outcome.try_timeout());
        assert!(waker.claim_timeout());

        let server = transport(BASE_B);
        assert_eq!(reply.reply(&server).unwrap_err(), IpcError::PeerClosed);
        assert_eq!(outcome.raw(), OutcomeSlot::TIMEDOUT);
    }
}
