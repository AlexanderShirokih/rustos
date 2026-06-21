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

/// Роль заблокированной стороны - определяет, как её будят при встрече.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaiterKind {
    /// `send`: после доставки отправитель разблокируется немедленно.
    Send,
    /// `call`: вызывающая сторона остаётся блокированной до `reply`.
    Call,
    /// `recv`: получатель ждёт отправителя/вызывателя.
    Recv,
}

/// Исход операции, прочитанный разбуженной стороной из [`OutcomeSlot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendezvousOutcome {
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
pub enum BufferAccess {
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
    pub access: BufferAccess,
    pub handle_table: Arc<MutexCell<HandleTable>>,
    /// Значок port-хендла отправителя (`0` - без значка / не отправитель).
    pub badge: u64,
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

/// Атомарный слот исхода rendezvous. Несёт и терминальный исход доставки, и
/// (для `call`) арбитраж гонки "вызыватель-тайм-аут против сервер-reply".
///
/// Машина состояний:
///
/// ```text
///                    set()              (send/recv-матчер, cancel)
///   PENDING ───────────────────────────► DELIVERED / PEER_GONE / TRANSFER_FAILED
///      │
///      │ set_awaiting_reply()  (call-матчер: запрос доставлен)
///      ▼
///   AWAITING_REPLY ──try_timeout()──► TIMEDOUT       (вызыватель ушёл по тайм-ауту)
///      │
///      │ try_begin_reply()   (сервер закоммитил reply)
///      ▼
///   DELIVERING ──set()──► DELIVERED / PEER_GONE / TRANSFER_FAILED
/// ```
///
/// Для `send`/`recv` слот живёт только в ветке `PENDING -> терминал`; промежу-
/// точные состояния использует исключительно reply-фаза `call`.
pub struct OutcomeSlot {
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

/// Запись о заблокированном на port потоке.
struct Waiter {
    kind: WaiterKind,
    transport: ThreadTransport,
    waker: Arc<ParkWaker>,
    outcome: Arc<OutcomeSlot>,
    reply_slot: Arc<ReplySlot>,
}

/// Очередь ожидания: либо отправители/вызыватели, либо получатели, но не оба.
enum Queue {
    Empty,
    Senders(VecDeque<Waiter>),
    Receivers(VecDeque<Waiter>),
}

/// Port - объект, реализующий синхронный обмен сообщениями.
pub struct Port {
    inner: MutexCell<Queue>,
}

/// Действие caller после операции.
pub(crate) enum PortAction {
    /// Текущий поток обязан заблокироваться на `waker.state()`.
    Park {
        waker: Arc<ParkWaker>,
        outcome: Arc<OutcomeSlot>,
        reply_slot: Option<Arc<ReplySlot>>,
    },
    
    /// Операция завершилась немедленно, без блокировки текущего потока.
    Done { reply: Option<Arc<Reply>> },
    
    /// Ошибка (перенос caps/копирование тела провалились).
    Failed(IpcError),
}

impl Port {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MutexCell::new(Queue::Empty),
        })
    }

    /// Транспорт отправки текущего потока. 
    /// Для `call` caller обязан передать предсозданные `(waker, outcome)`, на которые
    /// будет ссылаться Reply.
    pub(crate) fn send_or_call(
        self: &Arc<Self>,
        kind: WaiterKind,
        sender: ThreadTransport,
        runtime: &Arc<dyn KernelRuntime>,
        call_ctx: Option<(Arc<ParkWaker>, Arc<OutcomeSlot>)>,
    ) -> PortAction {
        debug_assert!(matches!(kind, WaiterKind::Send | WaiterKind::Call));

        let receiver = self.inner.with_lock(pop_receiver);

        if let Some(receiver) = receiver {
            // current=sender, parked=receiver.
            match transfer_rendezvous(&sender, &receiver.transport) {
                Ok(()) => {
                    match kind {
                        WaiterKind::Send => {
                            receiver.outcome.set(RendezvousOutcome::Delivered);
                            receiver.waker.signal_match();
                            PortAction::Done { reply: None }
                        }
                        WaiterKind::Call => {
                            // вызывающая сторона паркуется до reply; Reply
                            // ссылается на его предсозданные waker/outcome и
                            // отдаётся получателю через его reply_slot.
                            let (waker, outcome) =
                                call_ctx.expect("call requires preallocated park ctx");
                            outcome.set_awaiting_reply();
                            
                            let reply = Reply::new(sender.clone(), waker.clone(), outcome.clone());
                            receiver.reply_slot.install(reply);
                            receiver.outcome.set(RendezvousOutcome::Delivered);
                            receiver.waker.signal_match();
                            PortAction::Park {
                                waker,
                                outcome,
                                reply_slot: None,
                            }
                        }
                        WaiterKind::Recv => unreachable!(),
                    }
                }
                Err(e) => {
                    receiver.outcome.set(RendezvousOutcome::TransferFailed);
                    receiver.waker.signal_match();
                    PortAction::Failed(e)
                }
            }
        } else {
            // Никто не ждёт - паркуем текущий поток как sender/caller.
            let (waker, outcome) = if kind == WaiterKind::Call {
                call_ctx.expect("call requires preallocated park ctx")
            } else {
                let token = runtime.current_wait_token();
                (
                    Arc::new(ParkWaker::new(runtime.clone(), token)),
                    OutcomeSlot::new(),
                )
            };
            self.enqueue_sender(kind, sender, &waker, &outcome);
            PortAction::Park {
                waker,
                outcome,
                reply_slot: None,
            }
        }
    }

    /// Транспорт приемки текущего потока.
    pub(crate) fn recv(
        self: &Arc<Self>,
        receiver: ThreadTransport,
        runtime: &Arc<dyn KernelRuntime>,
    ) -> PortAction {
        let sender = self.inner.with_lock(pop_sender);

        if let Some(sender) = sender {
            // current=receiver, parked=sender/caller.
            match transfer_rendezvous(&sender.transport, &receiver) {
                Ok(()) => match sender.kind {
                    WaiterKind::Send => {
                        sender.outcome.set(RendezvousOutcome::Delivered);
                        sender.waker.signal_match();
                        PortAction::Done { reply: None }
                    }
                    WaiterKind::Call => {
                        // Запрос доставлен - переводим вызывателя в ожидание
                        // reply до выдачи Reply серверу.
                        sender.outcome.set_awaiting_reply();
                        let reply = Reply::new(
                            sender.transport.clone(),
                            sender.waker.clone(),
                            sender.outcome.clone(),
                        );
                        PortAction::Done { reply: Some(reply) }
                    }
                    WaiterKind::Recv => unreachable!("receiver found in senders queue"),
                },
                Err(e) => {
                    sender.outcome.set(RendezvousOutcome::TransferFailed);
                    sender.waker.signal_match();
                    PortAction::Failed(e)
                }
            }
        } else {
            let token = runtime.current_wait_token();
            let waker = Arc::new(ParkWaker::new(runtime.clone(), token));
            let outcome = OutcomeSlot::new();
            let reply_slot = ReplySlot::new();
            self.enqueue_receiver(receiver, &waker, &outcome, &reply_slot);
            PortAction::Park {
                waker,
                outcome,
                reply_slot: Some(reply_slot),
            }
        }
    }

    /// Будит всех ждущих с исходом `PeerGone`. Вызывается при закрытии
    /// последнего хендла (через cancel-target) и на Drop.
    pub fn cancel_all(&self) {
        let drained = self
            .inner
            .with_lock(|q| core::mem::replace(q, Queue::Empty));
        let waiters = match drained {
            Queue::Empty => return,
            Queue::Senders(ss) | Queue::Receivers(ss) => ss,
        };
        for w in waiters {
            w.outcome.set(RendezvousOutcome::PeerGone);
            w.waker.cancel();
        }
    }

    /// Снимает с очереди waiter по идентичности `waker`. 
    /// Возвращает `true`, если он был найден и удалён.
    pub(crate) fn remove_waiter(&self, waker: &Arc<ParkWaker>) -> bool {
        self.inner.with_lock(|q| {
            let dq = match q {
                Queue::Empty => return false,
                Queue::Senders(dq) | Queue::Receivers(dq) => dq,
            };
            let Some(pos) = dq.iter().position(|w| Arc::ptr_eq(&w.waker, waker)) else {
                return false;
            };
            dq.remove(pos);
            if dq.is_empty() {
                *q = Queue::Empty;
            }
            true
        })
    }

    fn enqueue_sender(
        &self,
        kind: WaiterKind,
        transport: ThreadTransport,
        waker: &Arc<ParkWaker>,
        outcome: &Arc<OutcomeSlot>,
    ) {
        let waiter = Waiter {
            kind,
            transport,
            waker: waker.clone(),
            outcome: outcome.clone(),
            reply_slot: ReplySlot::new(),
        };
        self.inner.with_lock(|q| match q {
            Queue::Empty => {
                let mut dq = VecDeque::new();
                dq.push_back(waiter);
                *q = Queue::Senders(dq);
            }
            Queue::Senders(ss) => ss.push_back(waiter),
            Queue::Receivers(_) => unreachable!("queue switched under single lock"),
        });
    }

    fn enqueue_receiver(
        &self,
        transport: ThreadTransport,
        waker: &Arc<ParkWaker>,
        outcome: &Arc<OutcomeSlot>,
        reply_slot: &Arc<ReplySlot>,
    ) {
        let waiter = Waiter {
            kind: WaiterKind::Recv,
            transport,
            waker: waker.clone(),
            outcome: outcome.clone(),
            reply_slot: reply_slot.clone(),
        };
        self.inner.with_lock(|q| match q {
            Queue::Empty => {
                let mut dq = VecDeque::new();
                dq.push_back(waiter);
                *q = Queue::Receivers(dq);
            }
            Queue::Receivers(rs) => rs.push_back(waiter),
            Queue::Senders(_) => unreachable!("queue switched under single lock"),
        });
    }

    #[cfg(test)]
    pub(crate) fn queue_is_empty(&self) -> bool {
        self.inner.with_lock(|q| matches!(q, Queue::Empty))
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
    match port.send_or_call(WaiterKind::Send, sender, runtime, None) {
        PortAction::Done { .. } => Ok(()),
        PortAction::Failed(e) => Err(e),
        PortAction::Park { waker, outcome, .. } => {
            park_with_timeout(runtime, &waker, timeout_ns);
            resolve_send_recv(port, runtime, &waker, &outcome)
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
    let token = runtime.current_wait_token();
    let waker = Arc::new(ParkWaker::new(runtime.clone(), token));
    let outcome = OutcomeSlot::new();
    match port.send_or_call(
        WaiterKind::Call,
        caller,
        runtime,
        Some((waker.clone(), outcome.clone())),
    ) {
        PortAction::Failed(e) => Err(e),
        PortAction::Done { .. } => {
            // call никогда не завершается синхронно (всегда Park).
            unreachable!("call always parks until reply");
        }
        PortAction::Park { waker, outcome, .. } => {
            park_with_timeout(runtime, &waker, timeout_ns);
            resolve_call(port, runtime, &waker, &outcome, timeout_ns)
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
        PortAction::Done { reply } => Ok(reply),
        PortAction::Failed(e) => Err(e),
        PortAction::Park {
            waker,
            outcome,
            reply_slot,
        } => {
            park_with_timeout(runtime, &waker, timeout_ns);
            resolve_send_recv(port, runtime, &waker, &outcome)?;
            // Доставка состоялась; если матчер положил Reply - забираем.
            Ok(reply_slot.and_then(|slot| slot.take()))
        }
    }
}

fn pop_receiver(q: &mut Queue) -> Option<Waiter> {
    if let Queue::Receivers(rs) = q {
        let w = rs.pop_front();
        if rs.is_empty() {
            *q = Queue::Empty;
        }
        w
    } else {
        None
    }
}

fn pop_sender(q: &mut Queue) -> Option<Waiter> {
    if let Queue::Senders(ss) = q {
        let w = ss.pop_front();
        if ss.is_empty() {
            *q = Queue::Empty;
        }
        w
    } else {
        None
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        self.cancel_all();
    }
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
            HandleTable, ProcessObject, ThreadObject,
            errors::SpawnError,
            ipc_buffer_xfer::test_mapper::PageMapper,
            runtime::{KernelRuntime, UserThreadEntry, WaitToken},
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
        fn current_thread_object(&self) -> Option<Arc<ThreadObject>> {
            None
        }
        fn current_process_object(&self) -> Option<Arc<ProcessObject>> {
            None
        }
        fn exit_current_thread(&self, _c: i32) -> ! {
            unreachable!()
        }
        fn block_current_until(&self, _f: &AtomicU32, _t: Option<u64>) {}
        fn unblock(&self, _t: WaitToken) {
            self.unblocks.fetch_add(1, Ordering::AcqRel);
        }
        fn create_empty_process(&self, _n: &str) -> Result<Arc<ProcessObject>, SpawnError> {
            Err(SpawnError::NoFreeProcessSlots)
        }
        fn create_user_thread(
            &self,
            _p: &Arc<ProcessObject>,
            _e: UserThreadEntry,
        ) -> Result<Arc<ThreadObject>, SpawnError> {
            Err(SpawnError::NoFreeThreadSlots)
        }
        fn terminate_thread(&self, _t: &Arc<ThreadObject>, _c: i32) -> Result<(), IpcError> {
            Ok(())
        }
        fn terminate_process(&self, _p: &Arc<ProcessObject>, _c: i32) -> Result<(), IpcError> {
            Ok(())
        }
        fn load_user_image_into(
            &self,
            _p: &Arc<ProcessObject>,
            _i: &crate::UserImageInstall,
        ) -> Result<(), crate::LoadImageError> {
            Err(crate::LoadImageError::ProcessNotFound)
        }
        fn start_user_process(
            &self,
            _p: &Arc<ProcessObject>,
            _s: crate::UserStartSpec,
        ) -> Result<Arc<ThreadObject>, crate::StartProcessError> {
            Err(crate::StartProcessError::ProcessNotFound)
        }
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
        assert!(matches!(action, PortAction::Park { .. }));
        assert!(!ep.queue_is_empty());
    }

    #[test]
    fn send_matches_parked_receiver_and_delivers() {
        let rt = StubRuntime::arc();
        let ep = Port::new();

        // Получатель паркуется.
        let recv_action = ep.recv(transport(BASE_B), &rt);
        let PortAction::Park {
            outcome: recv_outcome,
            reply_slot: recv_reply_slot,
            ..
        } = recv_action
        else {
            panic!("recv must park");
        };
        assert!(recv_outcome.get().is_none());

        // Отправитель встречает получателя: матч, доставка, queue пуст.
        let send_action = ep.send_or_call(WaiterKind::Send, transport(BASE_A), &rt, None);
        assert!(matches!(send_action, PortAction::Done { reply: None }));
        assert_eq!(recv_outcome.get(), Some(RendezvousOutcome::Delivered));
        assert!(ep.queue_is_empty());
        // send -> recv: reply отсутствует.
        assert!(recv_reply_slot.unwrap().take().is_none());
    }

    #[test]
    fn send_with_no_receiver_parks_as_sender() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let action = ep.send_or_call(WaiterKind::Send, transport(BASE_A), &rt, None);
        assert!(matches!(action, PortAction::Park { .. }));
        assert!(!ep.queue_is_empty());
    }

    #[test]
    fn recv_matches_parked_sender() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let send_action = ep.send_or_call(WaiterKind::Send, transport(BASE_A), &rt, None);
        let PortAction::Park {
            outcome: send_outcome,
            ..
        } = send_action
        else {
            panic!("send must park");
        };

        let recv_action = ep.recv(transport(BASE_B), &rt);
        assert!(matches!(recv_action, PortAction::Done { reply: None }));
        assert_eq!(send_outcome.get(), Some(RendezvousOutcome::Delivered));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn call_matched_by_recv_yields_reply() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        // вызывающая сторона паркуется (call всегда паркуется).
        let waker = Arc::new(ParkWaker::new(rt.clone(), rt.current_wait_token()));
        let outcome = OutcomeSlot::new();
        let call_action = ep.send_or_call(
            WaiterKind::Call,
            transport(BASE_A),
            &rt,
            Some((waker, outcome.clone())),
        );
        assert!(matches!(call_action, PortAction::Park { .. }));

        // Получатель смэтчил call -> получает Reply, вызывающая сторона остаётся
        // заблокированным (outcome ещё pending).
        let recv_action = ep.recv(transport(BASE_B), &rt);
        let PortAction::Done { reply: Some(reply) } = recv_action else {
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
        let PortAction::Park { outcome, .. } = action else {
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
        let PortAction::Park { outcome, .. } = action else {
            panic!("recv must park");
        };
        drop(ep);
        assert_eq!(outcome.get(), Some(RendezvousOutcome::PeerGone));
    }

    #[test]
    fn remove_waiter_is_arbiter_and_idempotent() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let PortAction::Park { waker, .. } = ep.recv(transport(BASE_B), &rt) else {
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
        let PortAction::Park {
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
        let waker = Arc::new(ParkWaker::new(rt.clone(), rt.current_wait_token()));
        let outcome = OutcomeSlot::new();
        let call_action = ep.send_or_call(
            WaiterKind::Call,
            transport(BASE_A),
            &rt,
            Some((waker.clone(), outcome.clone())),
        );
        assert!(matches!(call_action, PortAction::Park { .. }));

        let PortAction::Done { reply: Some(reply) } = ep.recv(transport(BASE_B), &rt) else {
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
