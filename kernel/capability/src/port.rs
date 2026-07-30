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
//!
//! Порт считает своих держателей: получателей (capability с правом `READ`) и отправителей каждого
//! значка (capability со значком и правом `WRITE`). Учёт живёт под локом очереди, поэтому
//! изменение множества и разбор очереди атомарны. Операция удерживает учёт своей стороны от
//! снимка capability до доставки или постановки в очередь ([`OperationHold`]).

use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use collections::{LockCell, MutexCell};
use memory::{memory_mapper::MemoryMapper, virtual_address::VirtualAddress};
use syscall::Rights;

use super::{
    errors::IpcError,
    handle_table::HandleTable,
    ipc_buffer_xfer::{transfer_rendezvous, write_badge},
    reply::{Reply, ReplySlot},
    rev_node::RevocationHook,
    runtime::{KernelRuntime, ParkState},
    target::{HolderTicket, OperationHold},
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
    /// Отправители значка исчезли: кадра нет, получателю доставлен значок.
    SenderGone(u64),
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
    badge: AtomicU64,
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
    /// Отправители значка исчезли; значок лежит в `badge`.
    pub(crate) const SENDER_GONE: u32 = 7;

    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU32::new(Self::PENDING),
            badge: AtomicU64::new(0),
        })
    }

    pub(crate) fn set(&self, outcome: RendezvousOutcome) {
        let v = match outcome {
            RendezvousOutcome::Delivered => Self::DELIVERED,
            RendezvousOutcome::PeerGone => Self::PEER_GONE,
            RendezvousOutcome::TransferFailed => Self::TRANSFER_FAILED,
            RendezvousOutcome::SenderGone(badge) => {
                self.badge.store(badge, Ordering::Release);
                Self::SENDER_GONE
            }
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
            Self::SENDER_GONE => Some(RendezvousOutcome::SenderGone(
                self.badge.load(Ordering::Acquire),
            )),
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

/// Учётная запись держателей одного значка. Значения меняются только под локом порта.
pub(super) struct BadgeRecord {
    badge: u64,
    holders: AtomicU32,
}

impl BadgeRecord {
    fn new(badge: u64) -> Arc<Self> {
        Arc::new(Self {
            badge,
            holders: AtomicU32::new(0),
        })
    }

    fn holders(&self) -> u32 {
        self.holders.load(Ordering::Relaxed)
    }

    fn add_holder(&self) {
        self.holders.store(self.holders() + 1, Ordering::Relaxed);
    }

    fn remove_holder(&self) -> u32 {
        let left = self.holders().saturating_sub(1);
        self.holders.store(left, Ordering::Relaxed);
        left
    }
}

/// Учёт одного экземпляра [`Capability`](super::Capability) в множествах порта.
struct PortHolder {
    port: Arc<Port>,
    receiver: bool,
    badge: Option<Arc<BadgeRecord>>,
    released: AtomicBool,
}

impl HolderTicket for PortHolder {
    fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        self.port.release_holder(self.receiver, self.badge.as_ref());
    }

    fn reacquire(&self) -> Option<Arc<dyn HolderTicket>> {
        self.port.reacquire_holder(self)
    }
}

impl RevocationHook for PortHolder {
    fn revoke(&self) {
        self.release();
    }
}

/// Снятый с очереди waiter любой из двух ролей: под локом порта не роняется.
enum QueuedWaiter {
    Sender(SenderWaiter),
    Receiver(ReceiverWaiter),
}

impl QueuedWaiter {
    fn waker(&self) -> &Arc<ParkWaker> {
        match self {
            Self::Sender(waiter) => &waiter.waker,
            Self::Receiver(waiter) => &waiter.waker,
        }
    }
}

/// Снятый с очереди waiter с уже опубликованным исходом: пробуждение и разрушение под локом порта
/// не выполняются.
enum PendingWake {
    /// Событие доставлено: значок ушедшей стороны лежит в слоте исхода.
    Delivered(ReceiverWaiter),
    /// Встречи не будет.
    Broken(QueuedWaiter),
}

impl PendingWake {
    fn wake(self) {
        match self {
            Self::Delivered(waiter) => waiter.waker.signal_match(),
            Self::Broken(waiter) => waiter.waker().cancel(),
        }
    }
}

/// Что достаётся получателю из состояния порта.
enum RecvPick {
    /// Ушедший значок с непрочтённым событием.
    DepartedBadge(u64),
    /// Встречный отправитель или вызыватель, снятый с очереди.
    Sender(SenderWaiter),
    /// Ни события, ни отправителя.
    Nothing,
}

// Потолок непрочтённых событий об уходе: на нём порт не заводит записей под новые значки, а `recv`
// события освобождает запись.
const DEPARTED_BADGE_LIMIT: usize = HandleTable::DEFAULT_CAPACITY as usize;

/// Очередь ожидания и учёт держателей под одним локом: `receivers` - живые capability с правом
/// `READ`, `badges` - записи значков, `departed` - записи с непрочтённым событием об уходе.
struct PortState {
    queue: Queue,
    receivers: usize,
    badges: Vec<Arc<BadgeRecord>>,
    departed: usize,
}

impl PortState {
    fn new() -> Self {
        Self {
            queue: Queue::Empty,
            receivers: 0,
            badges: Vec::new(),
            departed: 0,
        }
    }

    // Оживление записи с непрочтённым событием проходит и на потолке: потолком ограничены только
    // новые записи.
    fn register_badge(&mut self, badge: u64) -> Result<Arc<BadgeRecord>, IpcError> {
        if let Some(index) = self.badges.iter().position(|r| r.badge == badge) {
            let record = self.badges[index].clone();
            if record.holders() == 0 {
                self.departed -= 1;
            }
            record.add_holder();
            return Ok(record);
        }
        if self.departed >= DEPARTED_BADGE_LIMIT {
            return Err(IpcError::ResourceExhausted);
        }
        let record = BadgeRecord::new(badge);
        record.add_holder();
        self.badges.push(record.clone());
        Ok(record)
    }

    fn release_badge(&mut self, record: &Arc<BadgeRecord>, wakes: &mut Vec<PendingWake>) {
        if record.remove_holder() > 0 {
            return;
        }
        for waiter in self.take_senders_with_badge(record.badge) {
            waiter.outcome.set(RendezvousOutcome::PeerGone);
            wakes.push(PendingWake::Broken(QueuedWaiter::Sender(waiter)));
        }
        match self.pop_receiver() {
            Some(receiver) => {
                receiver
                    .outcome
                    .set(RendezvousOutcome::SenderGone(record.badge));
                wakes.push(PendingWake::Delivered(receiver));
                self.forget_badge(record);
            }
            None => self.departed += 1,
        }
    }

    fn release_receiver(&mut self, wakes: &mut Vec<PendingWake>) {
        debug_assert!(self.receivers > 0, "receiver accounting underflow");
        self.receivers = self.receivers.saturating_sub(1);
        if self.receivers == 0 {
            self.break_all(wakes);
        }
    }

    fn take_departed_badge(&mut self) -> Option<u64> {
        if self.departed == 0 {
            return None;
        }
        let index = self.badges.iter().position(|r| r.holders() == 0);
        debug_assert!(
            index.is_some(),
            "departed counts badge records without holders"
        );
        let index = index?;
        self.departed -= 1;
        Some(self.badges.swap_remove(index).badge)
    }

    fn forget_badge(&mut self, record: &Arc<BadgeRecord>) {
        if let Some(index) = self.badges.iter().position(|r| Arc::ptr_eq(r, record)) {
            self.badges.swap_remove(index);
        }
    }

    fn take_receiver(&mut self) -> Result<Option<ReceiverWaiter>, IpcError> {
        if self.receivers == 0 {
            return Err(IpcError::PeerClosed);
        }
        Ok(self.pop_receiver())
    }

    // Готовое событие об уходе идёт впереди очереди отправителей.
    fn pick_for_receiver(&mut self) -> RecvPick {
        if let Some(badge) = self.take_departed_badge() {
            return RecvPick::DepartedBadge(badge);
        }
        match self.pop_sender() {
            Some(sender) => RecvPick::Sender(sender),
            None => RecvPick::Nothing,
        }
    }

    fn break_all(&mut self, wakes: &mut Vec<PendingWake>) {
        match core::mem::replace(&mut self.queue, Queue::Empty) {
            Queue::Empty => {}
            Queue::Senders(senders) => break_each(senders, QueuedWaiter::Sender, wakes),
            Queue::Receivers(receivers) => break_each(receivers, QueuedWaiter::Receiver, wakes),
        }
    }

    fn take_senders_with_badge(&mut self, badge: u64) -> Vec<SenderWaiter> {
        let Queue::Senders(queued) = &mut self.queue else {
            return Vec::new();
        };
        let mut taken = Vec::new();
        let mut kept = VecDeque::with_capacity(queued.len());
        while let Some(waiter) = queued.pop_front() {
            if waiter.transport.badge == badge {
                taken.push(waiter);
            } else {
                kept.push_back(waiter);
            }
        }
        *queued = kept;
        self.collapse_if_empty();
        taken
    }

    // Единственный источник инварианта "пустой дек -> состояние `Empty`".
    fn collapse_if_empty(&mut self) {
        let drained = match &self.queue {
            Queue::Empty => false,
            Queue::Senders(queued) => queued.is_empty(),
            Queue::Receivers(queued) => queued.is_empty(),
        };
        if drained {
            self.queue = Queue::Empty;
        }
    }

    fn pop_receiver(&mut self) -> Option<ReceiverWaiter> {
        let waiter = match &mut self.queue {
            Queue::Receivers(receivers) => receivers.pop_front(),
            Queue::Empty | Queue::Senders(_) => None,
        };
        self.collapse_if_empty();
        waiter
    }

    fn pop_sender(&mut self) -> Option<SenderWaiter> {
        let waiter = match &mut self.queue {
            Queue::Senders(senders) => senders.pop_front(),
            Queue::Empty | Queue::Receivers(_) => None,
        };
        self.collapse_if_empty();
        waiter
    }

    // Err - waiter не поставлен: встал встречный получатель либо опустело их множество.
    fn try_push_sender(&mut self, waiter: SenderWaiter) -> Result<(), SenderWaiter> {
        if self.receivers == 0 {
            return Err(waiter);
        }
        match &mut self.queue {
            Queue::Senders(senders) => {
                senders.push_back(waiter);
                Ok(())
            }
            Queue::Empty => {
                let mut queued = VecDeque::new();
                queued.push_back(waiter);
                self.queue = Queue::Senders(queued);
                Ok(())
            }
            Queue::Receivers(_) => Err(waiter),
        }
    }

    // Err - waiter не поставлен: встал встречный отправитель либо готово событие об уходе значка.
    fn try_push_receiver(&mut self, waiter: ReceiverWaiter) -> Result<(), ReceiverWaiter> {
        if self.departed > 0 {
            return Err(waiter);
        }
        match &mut self.queue {
            Queue::Receivers(receivers) => {
                receivers.push_back(waiter);
                Ok(())
            }
            Queue::Empty => {
                let mut queued = VecDeque::new();
                queued.push_back(waiter);
                self.queue = Queue::Receivers(queued);
                Ok(())
            }
            Queue::Senders(_) => Err(waiter),
        }
    }

    fn take_by_waker(&mut self, waker: &Arc<ParkWaker>) -> Option<QueuedWaiter> {
        let removed = match &mut self.queue {
            Queue::Empty => None,
            Queue::Senders(senders) => take_by_waker(senders, waker).map(QueuedWaiter::Sender),
            Queue::Receivers(receivers) => {
                take_by_waker(receivers, waker).map(QueuedWaiter::Receiver)
            }
        };
        self.collapse_if_empty();
        removed
    }
}

/// Port - объект, реализующий синхронный обмен сообщениями.
pub struct Port {
    inner: MutexCell<PortState>,
}

/// Снимок port-capability для одной операции: объект, значок хендла и удержание учёта его стороны.
pub struct PortAccess {
    pub port: Arc<Port>,
    pub badge: u64,
    pub hold: Option<OperationHold>,
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
    /// Ошибка (перенос caps/копирование тела провалились, множество получателей пусто).
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
    /// Получателю досталось событие об уходе значка, а не кадр.
    SenderGone {
        badge: u64,
    },
    Failed(IpcError),
}

impl Port {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MutexCell::new(PortState::new()),
        })
    }

    /// Учитывает нового держателя capability без значка: право `READ` вводит её в множество
    /// получателей, без этого права учёта нет.
    pub(super) fn register_holder(
        self: &Arc<Self>,
        rights: Rights,
    ) -> Option<Arc<dyn HolderTicket>> {
        if !rights.contains(Rights::READ) {
            return None;
        }
        self.inner.with_lock(|state| state.receivers += 1);
        Some(self.holder_ticket(true, None))
    }

    /// Учитывает нового держателя capability со значком: право `WRITE` вводит её в множество
    /// отправителей значка, право `READ` - в множество получателей. `ResourceExhausted` - порт не
    /// заводит записи под новый значок.
    pub(super) fn register_badged_holder(
        self: &Arc<Self>,
        rights: Rights,
        badge: NonZeroU64,
    ) -> Result<Option<Arc<dyn HolderTicket>>, IpcError> {
        if !rights.contains(Rights::WRITE) {
            return Ok(self.register_holder(rights));
        }
        let receiver = rights.contains(Rights::READ);
        // Отказ учёта значка оставляет множество получателей нетронутым.
        let record = self.inner.with_lock(|state| {
            let record = state.register_badge(badge.get())?;
            if receiver {
                state.receivers += 1;
            }
            Ok(record)
        })?;
        Ok(Some(self.holder_ticket(receiver, Some(record))))
    }

    // Снятый учёт вторым токеном не оживает: `release` выставляет флаг до захвата лока, поэтому
    // сброшенный под локом флаг означает живого держателя.
    fn reacquire_holder(self: &Arc<Self>, holder: &PortHolder) -> Option<Arc<dyn HolderTicket>> {
        let counted = self.inner.with_lock(|state| {
            if holder.released.load(Ordering::Acquire) {
                return false;
            }
            if holder.receiver {
                state.receivers += 1;
            }
            if let Some(record) = &holder.badge {
                record.add_holder();
            }
            true
        });
        counted.then(|| self.holder_ticket(holder.receiver, holder.badge.clone()))
    }

    fn holder_ticket(
        self: &Arc<Self>,
        receiver: bool,
        badge: Option<Arc<BadgeRecord>>,
    ) -> Arc<dyn HolderTicket> {
        Arc::new(PortHolder {
            port: self.clone(),
            receiver,
            badge,
            released: AtomicBool::new(false),
        })
    }

    // Пробуждения опустевших множеств под локом порта не выполняются.
    fn release_holder(&self, receiver: bool, badge: Option<&Arc<BadgeRecord>>) {
        let wakes = self.inner.with_lock(|state| {
            let mut wakes = Vec::new();
            if receiver {
                state.release_receiver(&mut wakes);
            }
            if let Some(record) = badge {
                state.release_badge(record, &mut wakes);
            }
            wakes
        });
        for wake in wakes {
            wake.wake();
        }
    }

    /// `send` текущего потока: доставляет ждущему получателю либо паркуется. `hold` держит учёт
    /// значка отправителя до доставки или постановки в очередь.
    ///
    /// Матч и постановка в очередь - разные захваты queue-лока: между ними берётся wait-token.
    /// Изменившееся в это окно состояние отвергает постановку, и цикл повторяет матч.
    pub(crate) fn send(
        &self,
        sender: &ThreadTransport,
        runtime: &Arc<dyn KernelRuntime>,
        hold: Option<OperationHold>,
    ) -> SendAction {
        let action = loop {
            match self.inner.with_lock(PortState::take_receiver) {
                Err(e) => break SendAction::Failed(e),
                Ok(Some(receiver)) => break deliver_send(sender, &receiver),
                Ok(None) => {}
            }

            let ctx = ParkCtx::for_current(runtime);
            let waiter = Waiter {
                kind: SendKind::Send,
                transport: sender.clone(),
                waker: ctx.waker.clone(),
                outcome: ctx.outcome.clone(),
                reply_slot: ReplySlot::new(),
            };
            if self
                .inner
                .with_lock(|state| state.try_push_sender(waiter))
                .is_ok()
            {
                break SendAction::Park {
                    waker: ctx.waker,
                    outcome: ctx.outcome,
                };
            }
        };
        // Уход значка застаёт кадр доставленным либо waiter в очереди.
        drop(hold);
        action
    }

    /// `call` текущего потока с готовым контекстом парковки. Удержание и ретрай - как в
    /// [`Self::send`].
    pub(crate) fn call(
        &self,
        caller: &ThreadTransport,
        ctx: ParkCtx,
        hold: Option<OperationHold>,
    ) -> CallAction {
        let action = loop {
            match self.inner.with_lock(PortState::take_receiver) {
                Err(e) => break CallAction::Failed(e),
                Ok(Some(receiver)) => break deliver_call(caller, &receiver, ctx),
                Ok(None) => {}
            }

            let waiter = Waiter {
                kind: SendKind::Call,
                transport: caller.clone(),
                waker: ctx.waker.clone(),
                outcome: ctx.outcome.clone(),
                reply_slot: ReplySlot::new(),
            };
            if self
                .inner
                .with_lock(|state| state.try_push_sender(waiter))
                .is_ok()
            {
                break CallAction::Park {
                    waker: ctx.waker,
                    outcome: ctx.outcome,
                };
            }
        };
        drop(hold);
        action
    }

    /// `recv` текущего потока: забирает готовое событие или сообщение ждущего отправителя, либо
    /// паркуется. `hold` держит роль получателя до приёма или постановки в очередь, ретрай - как в
    /// [`Self::send`].
    pub(crate) fn recv(
        &self,
        receiver: &ThreadTransport,
        runtime: &Arc<dyn KernelRuntime>,
        hold: Option<OperationHold>,
    ) -> RecvAction {
        let action = loop {
            match self.inner.with_lock(PortState::pick_for_receiver) {
                RecvPick::DepartedBadge(badge) => break RecvAction::SenderGone { badge },
                RecvPick::Sender(sender) => break deliver_recv(sender, receiver),
                RecvPick::Nothing => {}
            }

            let ctx = ParkCtx::for_current(runtime);
            let reply_slot = ReplySlot::new();
            let waiter = Waiter {
                kind: (),
                transport: receiver.clone(),
                waker: ctx.waker.clone(),
                outcome: ctx.outcome.clone(),
                reply_slot: reply_slot.clone(),
            };
            if self
                .inner
                .with_lock(|state| state.try_push_receiver(waiter))
                .is_ok()
            {
                break RecvAction::Park {
                    waker: ctx.waker,
                    outcome: ctx.outcome,
                    reply_slot,
                };
            }
        };
        // Обнуление множества получателей застаёт приём завершённым либо waiter в очереди.
        drop(hold);
        action
    }

    /// Будит всех ждущих с исходом `PeerGone`.
    pub fn cancel_all(&self) {
        let wakes = self.inner.with_lock(|state| {
            let mut wakes = Vec::new();
            state.break_all(&mut wakes);
            wakes
        });
        for wake in wakes {
            wake.wake();
        }
    }

    /// Снимает с очереди waiter по идентичности `waker`.
    /// Возвращает `true`, если он был найден и удалён.
    pub(crate) fn remove_waiter(&self, waker: &Arc<ParkWaker>) -> bool {
        // Waiter не роняется под локом порта: его транспорт держит capability-таблицу.
        let removed = self.inner.with_lock(|state| state.take_by_waker(waker));
        removed.is_some()
    }

    #[cfg(test)]
    pub(crate) fn queue_is_empty(&self) -> bool {
        self.inner
            .with_lock(|state| matches!(state.queue, Queue::Empty))
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

fn deliver_call(caller: &ThreadTransport, receiver: &ReceiverWaiter, ctx: ParkCtx) -> CallAction {
    match transfer_rendezvous(caller, &receiver.transport) {
        Ok(()) => {
            ctx.outcome.set_awaiting_reply();
            let reply = Reply::new(caller.clone(), ctx.waker.clone(), ctx.outcome.clone());
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

fn break_each<K>(
    waiters: VecDeque<Waiter<K>>,
    role: fn(Waiter<K>) -> QueuedWaiter,
    wakes: &mut Vec<PendingWake>,
) {
    for waiter in waiters {
        waiter.outcome.set(RendezvousOutcome::PeerGone);
        wakes.push(PendingWake::Broken(role(waiter)));
    }
}

fn take_by_waker<K>(queued: &mut VecDeque<Waiter<K>>, waker: &Arc<ParkWaker>) -> Option<Waiter<K>> {
    let position = queued.iter().position(|w| Arc::ptr_eq(&w.waker, waker))?;
    queued.remove(position)
}

fn outcome_to_result(outcome: &Arc<OutcomeSlot>) -> Result<(), IpcError> {
    match outcome.get() {
        Some(RendezvousOutcome::Delivered) => Ok(()),
        Some(RendezvousOutcome::PeerGone) => Err(IpcError::PeerClosed),
        Some(RendezvousOutcome::TransferFailed) => Err(IpcError::BufferTooSmall),
        Some(RendezvousOutcome::SenderGone(_)) => Err(IpcError::SenderGone),
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

/// Реализует send из IPC-буфера отправителя. `hold` - удержание учёта его стороны, снимаемое после
/// доставки или постановки в очередь.
pub fn port_send(
    port: &Arc<Port>,
    sender: &ThreadTransport,
    runtime: &Arc<dyn KernelRuntime>,
    timeout_ns: Option<u64>,
    hold: Option<OperationHold>,
) -> Result<(), IpcError> {
    match port.send(sender, runtime, hold) {
        SendAction::Done => Ok(()),
        SendAction::Failed(e) => Err(e),
        SendAction::Park { waker, outcome } => {
            arm_port_cancel(port, runtime, &waker, &outcome, None);
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
/// семантика значений и `hold` - как у [`port_send`].
/// На истечении - [`IpcError::Timeout`].
pub fn port_call(
    port: &Arc<Port>,
    caller: &ThreadTransport,
    runtime: &Arc<dyn KernelRuntime>,
    timeout_ns: Option<u64>,
    hold: Option<OperationHold>,
) -> Result<(), IpcError> {
    // вызывающая сторона всегда паркуется до reply - предсоздаём park-ctx, чтобы
    // Reply мог на него ссылаться даже на синхронном матче.
    match port.call(caller, ParkCtx::for_current(runtime), hold) {
        CallAction::Failed(e) => Err(e),
        CallAction::Park { waker, outcome } => {
            arm_port_cancel(port, runtime, &waker, &outcome, None);
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

/// `recv`: блокирующая. Возвращает `Some(reply)`, если встречным был `call`. Семантика
/// `timeout_ns` и `hold` - как у [`port_send`], на истечении - [`IpcError::Timeout`].
/// [`IpcError::SenderGone`] - кадра нет, а в `IpcBuffer.badge` лежит значок, чьи отправители
/// исчезли.
pub fn port_recv(
    port: &Arc<Port>,
    receiver: &ThreadTransport,
    runtime: &Arc<dyn KernelRuntime>,
    timeout_ns: Option<u64>,
    hold: Option<OperationHold>,
) -> Result<Option<Arc<Reply>>, IpcError> {
    match port.recv(receiver, runtime, hold) {
        RecvAction::Done { reply } => Ok(reply),
        RecvAction::Failed(e) => Err(e),
        RecvAction::SenderGone { badge } => Err(publish_departed_badge(receiver, badge)),
        RecvAction::Park {
            waker,
            outcome,
            reply_slot,
        } => {
            arm_port_cancel(port, runtime, &waker, &outcome, Some(reply_slot.clone()));
            park_with_timeout(runtime, &waker, timeout_ns);
            let resolved = resolve_send_recv(port, runtime, &waker, &outcome);
            runtime.clear_blocked_cancel();
            if let Some(RendezvousOutcome::SenderGone(badge)) = outcome.get() {
                return Err(publish_departed_badge(receiver, badge));
            }
            resolved?;

            Ok(reply_slot.take())
        }
    }
}

// Отказ буфера теряет событие, как и провалившийся перенос кадра.
fn publish_departed_badge(receiver: &ThreadTransport, badge: u64) -> IpcError {
    match write_badge(receiver, badge) {
        Ok(()) => IpcError::SenderGone,
        Err(e) => e,
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
    /// Слот получателя: матчер `call` кладёт туда reply до пробуждения.
    reply_slot: Option<Arc<ReplySlot>>,
}

impl CancelTarget for PortCancel {
    fn cancel(&self) {
        self.port.remove_waiter(&self.waker);
        self.outcome.try_timeout();
    }

    fn take_abandoned_reply(&self) -> Option<Arc<Reply>> {
        self.reply_slot.as_ref().and_then(|slot| slot.take())
    }
}

fn arm_port_cancel(
    port: &Arc<Port>,
    runtime: &Arc<dyn KernelRuntime>,
    waker: &Arc<ParkWaker>,
    outcome: &Arc<OutcomeSlot>,
    reply_slot: Option<Arc<ReplySlot>>,
) {
    let cancel: Arc<dyn CancelTarget> = Arc::new(PortCancel {
        port: port.clone(),
        waker: waker.clone(),
        outcome: outcome.clone(),
        reply_slot,
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
            CancelTarget, DrainedHandles, HandleTable,
            handle::Capability,
            ipc_buffer_xfer::test_mapper::PageMapper,
            rev_node::revoke_subtree,
            runtime::{KernelRuntime, WaitToken},
            target::CapabilityTarget,
        },
        *,
    };

    const BASE_A: usize = 0x4000_0000;
    const BASE_B: usize = 0x5000_0000;
    const BADGE_A: u64 = 0xA1;
    const BADGE_B: u64 = 0xB2;

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

    /// Учтённый получатель: без живой capability с `READ` порт отвергает
    /// отправителей.
    fn receiver_capability(port: &Arc<Port>) -> Capability {
        Capability::new(CapabilityTarget::Port(port.clone()), Rights::READ)
    }

    /// Корневая capability сервера: получатель и источник минта клиентских.
    fn root_capability(port: &Arc<Port>) -> Capability {
        Capability::new(
            CapabilityTarget::Port(port.clone()),
            Rights::READ | Rights::WRITE | Rights::DUPLICATE,
        )
    }

    /// Клиентская capability со значком, выведенная из корневой.
    fn badged_capability(root: &Capability, badge: u64) -> Capability {
        root.duplicate(Rights::WRITE, badge)
            .expect("mint badged capability")
    }

    #[test]
    fn recv_with_no_sender_parks_as_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let action = ep.recv(&transport(BASE_B), &rt, None);
        assert!(matches!(action, RecvAction::Park { .. }));
        assert!(!ep.queue_is_empty());
    }

    #[test]
    fn send_matches_parked_receiver_and_delivers() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);

        // Получатель паркуется.
        let recv_action = ep.recv(&transport(BASE_B), &rt, None);
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
        let send_action = ep.send(&transport(BASE_A), &rt, None);
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
        let _receiver = receiver_capability(&ep);
        let action = ep.send(&transport(BASE_A), &rt, None);
        assert!(matches!(action, SendAction::Park { .. }));
        assert!(!ep.queue_is_empty());
    }

    #[test]
    fn recv_matches_parked_sender() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        let send_action = ep.send(&transport(BASE_A), &rt, None);
        let SendAction::Park {
            outcome: send_outcome,
            ..
        } = send_action
        else {
            panic!("send must park");
        };

        let recv_action = ep.recv(&transport(BASE_B), &rt, None);
        assert!(matches!(recv_action, RecvAction::Done { reply: None }));
        assert_eq!(send_outcome.get(), Some(RendezvousOutcome::Delivered));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn call_matched_by_recv_yields_reply() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        // вызывающая сторона паркуется (call всегда паркуется).
        let ctx = ParkCtx::for_current(&rt);
        let outcome = ctx.outcome.clone();
        let call_action = ep.call(&transport(BASE_A), ctx, None);
        assert!(matches!(call_action, CallAction::Park { .. }));

        // Получатель смэтчил call -> получает Reply, вызывающая сторона остаётся
        // заблокированным (outcome ещё pending).
        let recv_action = ep.recv(&transport(BASE_B), &rt, None);
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
        let action = ep.recv(&transport(BASE_B), &rt, None);
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
        let action = ep.recv(&transport(BASE_B), &rt, None);
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
        let RecvAction::Park { waker, outcome, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("recv must park");
        };
        assert!(!ep.queue_is_empty());

        // Терминация заблокированного потока: scheduler зовёт PortCancel,
        // снимающий Waiter с очереди - матчер позже не доставит в мёртвый поток.
        let cancel = PortCancel {
            port: ep.clone(),
            waker: waker.clone(),
            outcome: outcome.clone(),
            reply_slot: None,
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
        let _receiver = receiver_capability(&ep);
        // вызывающая сторона паркуется (call всегда паркуется до reply).
        let ctx = ParkCtx::for_current(&rt);
        let (waker, outcome) = (ctx.waker.clone(), ctx.outcome.clone());
        let call_action = ep.call(&transport(BASE_A), ctx, None);
        assert!(matches!(call_action, CallAction::Park { .. }));

        // Получатель забрал запрос -> вызывающая сторона в фазе 2 (AWAITING_REPLY); его
        // Waiter уже не в очереди, поэтому remove_waiter в cancel - no-op.
        let RecvAction::Done { reply: Some(reply) } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("recv must return a reply for call");
        };
        assert_eq!(outcome.raw(), OutcomeSlot::AWAITING_REPLY);

        // Терминация: PortCancel переводит слот в TIMEDOUT (фаза 2) - так
        // поздний reply сервера не пишет в освобождаемый IPC-буфер.
        let cancel = PortCancel {
            port: ep.clone(),
            waker: waker.clone(),
            outcome: outcome.clone(),
            reply_slot: None,
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
    fn port_cancel_releases_reply_abandoned_by_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        let RecvAction::Park {
            waker: receiver_waker,
            outcome: receiver_outcome,
            reply_slot,
        } = ep.recv(&transport(BASE_B), &rt, None)
        else {
            panic!("recv must park with a reply slot");
        };

        let caller_ctx = ParkCtx::for_current(&rt);
        let (caller_waker, caller_outcome) = (caller_ctx.waker.clone(), caller_ctx.outcome.clone());
        let call_action = ep.call(&transport(BASE_A), caller_ctx, None);
        assert!(matches!(call_action, CallAction::Park { .. }));
        assert_eq!(caller_outcome.raw(), OutcomeSlot::AWAITING_REPLY);

        // Терминация получателя между матчем и изъятием reply: Waiter уже снят
        // с очереди, и reply достижим только через слот.
        let cancel = PortCancel {
            port: ep.clone(),
            waker: receiver_waker,
            outcome: receiver_outcome,
            reply_slot: Some(reply_slot.clone()),
        };
        CancelTarget::cancel(&cancel);
        let abandoned = cancel
            .take_abandoned_reply()
            .expect("cancel yields the abandoned reply");
        // Слот опустошён: нормальный путь изъятия того же reply не получит.
        assert!(reply_slot.take().is_none());
        assert!(cancel.take_abandoned_reply().is_none());

        abandoned.cancel();
        assert_eq!(caller_outcome.get(), Some(RendezvousOutcome::PeerGone));
        assert_eq!(
            caller_waker.state().load(Ordering::Acquire),
            ParkState::CANCELED
        );
    }

    #[test]
    fn remove_waiter_is_arbiter_and_idempotent() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let RecvAction::Park { waker, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
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
        let _receiver = receiver_capability(&ep);
        // poll (Some(0)) без получателя не блокируется - сразу Timeout, и
        // отправитель снят с очереди.
        assert_eq!(
            port_send(&ep, &transport(BASE_A), &rt, Some(0), None),
            Err(IpcError::Timeout)
        );
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn port_recv_poll_without_sender_is_timeout() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        assert_eq!(
            port_recv(&ep, &transport(BASE_B), &rt, Some(0), None).map(|_| ()),
            Err(IpcError::Timeout)
        );
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn port_send_poll_matches_parked_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        // Получатель уже припаркован - poll-send встречает его немедленно.
        let RecvAction::Park {
            outcome: recv_outcome,
            ..
        } = ep.recv(&transport(BASE_B), &rt, None)
        else {
            panic!("recv must park");
        };
        assert_eq!(
            port_send(&ep, &transport(BASE_A), &rt, Some(0), None),
            Ok(())
        );
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
        let _receiver = receiver_capability(&ep);
        // вызывающая сторона паркуется (call всегда паркуется до reply).
        let ctx = ParkCtx::for_current(&rt);
        let (waker, outcome) = (ctx.waker.clone(), ctx.outcome.clone());
        let call_action = ep.call(&transport(BASE_A), ctx, None);
        assert!(matches!(call_action, CallAction::Park { .. }));

        let RecvAction::Done { reply: Some(reply) } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("recv must return a reply for call");
        };
        assert_eq!(outcome.raw(), OutcomeSlot::AWAITING_REPLY);

        assert!(outcome.try_timeout());
        assert!(waker.claim_timeout());

        let server = transport(BASE_B);
        assert_eq!(reply.reply(&server).unwrap_err(), IpcError::PeerClosed);
        assert_eq!(outcome.raw(), OutcomeSlot::TIMEDOUT);
    }

    #[test]
    fn send_and_call_without_receivers_are_refused_before_parking() {
        let rt = StubRuntime::arc();
        let ep = Port::new();

        let sent = ep.send(&transport(BASE_A), &rt, None);
        assert!(matches!(sent, SendAction::Failed(IpcError::PeerClosed)));
        assert!(ep.queue_is_empty());

        let called = ep.call(&transport(BASE_A), ParkCtx::for_current(&rt), None);
        assert!(matches!(called, CallAction::Failed(IpcError::PeerClosed)));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn last_receiver_capability_wakes_parked_sender() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let receiver = receiver_capability(&ep);
        let SendAction::Park { outcome, .. } = ep.send(&transport(BASE_A), &rt, None) else {
            panic!("send must park");
        };

        drop(receiver);

        assert_eq!(outcome.get(), Some(RendezvousOutcome::PeerGone));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn last_receiver_capability_wakes_parked_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let receiver = receiver_capability(&ep);
        let RecvAction::Park { outcome, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("recv must park");
        };

        // Бессрочная парковка дедлайна не имеет: получателя обязано разбудить
        // само обнуление множества.
        drop(receiver);

        assert_eq!(outcome.get(), Some(RendezvousOutcome::PeerGone));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn departed_badge_waits_for_the_next_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);

        // Получателя в момент ухода нет - событие ждёт очередного recv.
        drop(client);

        let picked = ep.recv(&transport(BASE_B), &rt, None);
        assert!(matches!(picked, RecvAction::SenderGone { badge } if badge == BADGE_A));
        // Событие одноразовое.
        assert!(matches!(
            ep.recv(&transport(BASE_B), &rt, None),
            RecvAction::Park { .. }
        ));
    }

    #[test]
    fn departed_badge_reaches_exactly_one_of_two_receivers() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);
        let RecvAction::Park { outcome: first, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("first recv must park");
        };
        let RecvAction::Park {
            outcome: second, ..
        } = ep.recv(&transport(BASE_B), &rt, None)
        else {
            panic!("second recv must park");
        };

        drop(client);

        let outcomes = [first.get(), second.get()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| matches!(o, Some(RendezvousOutcome::SenderGone(BADGE_A))))
                .count(),
            1
        );
        // Второй остался припаркованным - событие не размножается.
        assert!(outcomes.contains(&None));
        assert!(!ep.queue_is_empty());
    }

    #[test]
    fn departed_badge_precedes_a_parked_sender() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let live = badged_capability(&root, BADGE_A);
        let leaving = badged_capability(&root, BADGE_B);
        let SendAction::Park {
            outcome: sender, ..
        } = ep.send(&transport(BASE_A).with_badge(BADGE_A), &rt, None)
        else {
            panic!("send must park");
        };

        drop(leaving);

        // Событие идёт впереди очереди отправителей, начатый кадр не тронут.
        let picked = ep.recv(&transport(BASE_B), &rt, None);
        assert!(matches!(picked, RecvAction::SenderGone { badge } if badge == BADGE_B));
        assert!(sender.get().is_none());

        // Кадр забирает следующий recv.
        assert!(matches!(
            ep.recv(&transport(BASE_B), &rt, None),
            RecvAction::Done { reply: None }
        ));
        assert_eq!(sender.get(), Some(RendezvousOutcome::Delivered));
        drop(live);
    }

    #[test]
    fn departed_badge_removes_its_parked_senders() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);
        let SendAction::Park {
            outcome: sender, ..
        } = ep.send(&transport(BASE_A).with_badge(BADGE_A), &rt, None)
        else {
            panic!("send must park");
        };

        // Кадр снесённой сессии серверу не достаётся.
        drop(client);

        assert_eq!(sender.get(), Some(RendezvousOutcome::PeerGone));
        assert!(ep.queue_is_empty());
        let picked = ep.recv(&transport(BASE_B), &rt, None);
        assert!(matches!(picked, RecvAction::SenderGone { badge } if badge == BADGE_A));
    }

    #[test]
    fn minting_a_badge_again_retracts_its_unread_event() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);
        drop(client);

        let _revived = badged_capability(&root, BADGE_A);

        assert!(matches!(
            ep.recv(&transport(BASE_B), &rt, None),
            RecvAction::Park { .. }
        ));
    }

    #[test]
    fn badge_recycling_keeps_the_departure_ledger_consistent() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);

        // Уход без получателя, затем минт того же значения и второй уход - уже
        // при припаркованном получателе.
        drop(badged_capability(&root, BADGE_A));
        let revived = badged_capability(&root, BADGE_A);
        let RecvAction::Park { outcome, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("recv must park");
        };
        drop(revived);
        assert_eq!(outcome.get(), Some(RendezvousOutcome::SenderGone(BADGE_A)));

        // Непрочтённых событий не осталось - следующий recv паркуется.
        assert!(matches!(
            ep.recv(&transport(BASE_B), &rt, None),
            RecvAction::Park { .. }
        ));
        // Значение остаётся переиспользуемым.
        drop(badged_capability(&root, BADGE_A));
    }

    /// Уходы значков при пустой очереди получателей: каждый оставляет на порту
    /// запись с непрочтённым событием.
    fn accumulate_departed_badges(root: &Capability, count: u64) {
        for badge in 1..=count {
            drop(badged_capability(root, badge));
        }
    }

    /// Значок, для которого записи на порту с накопленными уходами заведомо нет.
    const EXTRA_BADGE: u64 = DEPARTED_BADGE_LIMIT as u64 + 1;

    #[test]
    fn a_new_badge_is_refused_when_departed_events_are_unread() {
        let ep = Port::new();
        let root = root_capability(&ep);
        accumulate_departed_badges(&root, DEPARTED_BADGE_LIMIT as u64);

        assert_eq!(
            root.duplicate(Rights::WRITE, EXTRA_BADGE).err(),
            Some(IpcError::ResourceExhausted)
        );
        // Значение с уже заведённой записью оживает и на потолке.
        let _revived = badged_capability(&root, 1);
    }

    #[test]
    fn reading_a_departed_badge_frees_its_record() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        accumulate_departed_badges(&root, DEPARTED_BADGE_LIMIT as u64);

        assert!(matches!(
            ep.recv(&transport(BASE_B), &rt, None),
            RecvAction::SenderGone { .. }
        ));
        let _fresh = badged_capability(&root, EXTRA_BADGE);
    }

    #[test]
    fn the_limit_counts_departed_badges_and_not_live_ones() {
        let ep = Port::new();
        let root = root_capability(&ep);

        // Записи с держателями потолком не ограничены.
        let live: Vec<Capability> = (1..=EXTRA_BADGE)
            .map(|badge| badged_capability(&root, badge))
            .collect();

        // Те же записи, оставшись без держателей, упирают порт в потолок.
        drop(live);
        assert_eq!(
            root.duplicate(Rights::WRITE, EXTRA_BADGE + 1).err(),
            Some(IpcError::ResourceExhausted)
        );
    }

    #[test]
    fn two_capabilities_of_one_badge_form_one_set() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let first = badged_capability(&root, BADGE_A);
        let second = badged_capability(&root, BADGE_A);

        drop(first);
        let RecvAction::Park { outcome, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("half-empty badge set raises no event");
        };

        // Уход объявляется, когда исчезла вторая capability того же значка.
        drop(second);
        assert_eq!(outcome.get(), Some(RendezvousOutcome::SenderGone(BADGE_A)));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn transfer_between_tables_keeps_holder_sets() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let mut source = HandleTable::new();
        let mut destination = HandleTable::new();
        let id = source
            .insert(Capability::new(
                CapabilityTarget::Port(ep.clone()),
                Rights::READ | Rights::TRANSFER,
            ))
            .expect("insert receiver capability");

        let mut drained = DrainedHandles::empty();
        source
            .try_drain_for_transfer(&[id], Rights::TRANSFER, &mut drained)
            .expect("transfer the receiver capability");
        let moved = drained.take_handles();
        drained.release();
        destination
            .insert(moved.into_iter().next().expect("single capability"))
            .expect("insert into destination");

        // Перенос переместил значение - множество получателей не изменилось.
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Park { .. }
        ));

        drop(destination);
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Failed(IpcError::PeerClosed)
        ));
    }

    #[test]
    fn revoked_capability_stops_counting_as_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let mut grantor = HandleTable::new();
        let mut recipient = HandleTable::new();
        let root = grantor
            .insert(Capability::new(
                CapabilityTarget::Port(ep.clone()),
                Rights::READ | Rights::DUPLICATE | Rights::TRANSFER,
            ))
            .expect("insert receiver capability");
        let derived = grantor
            .duplicate(root, Rights::READ | Rights::TRANSFER, 0)
            .expect("derive receiver capability");
        let mut drained = DrainedHandles::empty();
        grantor
            .try_drain_for_transfer(&[derived], Rights::TRANSFER, &mut drained)
            .expect("hand the derived capability over");
        let moved = drained.take_handles();
        drained.release();
        recipient
            .insert(moved.into_iter().next().expect("single capability"))
            .expect("insert into recipient");

        // Учёт снимает отзыв: производная капа жива у получателя, но её
        // держатель уже снят каскадом от закрытого корня.
        grantor
            .remove(root)
            .expect("remove the root capability")
            .release();
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Failed(IpcError::PeerClosed)
        ));

        // Дроп после отзыва учёта второй раз не снимает.
        drop(recipient);
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Failed(IpcError::PeerClosed)
        ));
    }

    #[test]
    fn a_port_lookup_hands_the_operation_its_hold() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let mut table = HandleTable::new();
        let id = table
            .insert(Capability::new(
                CapabilityTarget::Port(ep.clone()),
                Rights::READ,
            ))
            .expect("insert receiver capability");

        let access = table.get_port(id, Rights::READ).expect("port is readable");
        assert!(access.hold.is_some());

        // Пока удержание живо, закрытие последней capability множества получателей не обнуляет.
        table
            .remove(id)
            .expect("remove the receiver capability")
            .release();
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Park { .. }
        ));

        drop(access);
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Failed(IpcError::PeerClosed)
        ));
    }

    #[test]
    fn a_revoked_capability_hands_out_no_hold() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let receiver = receiver_capability(&ep);

        // Отзыв снял учёт этой capability, и удержание его не оживляет.
        revoke_subtree(receiver.node());

        assert!(receiver.hold_for_operation().is_none());
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Failed(IpcError::PeerClosed)
        ));
    }

    #[test]
    fn an_in_flight_recv_holds_its_role_until_it_is_queued() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let receiver = receiver_capability(&ep);

        // Последняя capability получателя закрывается, когда recv уже начат и ещё не в очереди.
        let hold = receiver.hold_for_operation();
        drop(receiver);

        let RecvAction::Park { outcome, .. } = ep.recv(&transport(BASE_B), &rt, hold) else {
            panic!("recv must park");
        };

        // Обнуление множества застало waiter в очереди, а не пустой порт.
        assert_eq!(outcome.get(), Some(RendezvousOutcome::PeerGone));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn an_in_flight_send_delivers_its_frame_before_its_badge_departs() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);
        let RecvAction::Park {
            outcome: server, ..
        } = ep.recv(&transport(BASE_B), &rt, None)
        else {
            panic!("recv must park");
        };

        // Последняя capability значка закрывается, когда send уже начат.
        let hold = client.hold_for_operation();
        drop(client);

        let sent = ep.send(&transport(BASE_A).with_badge(BADGE_A), &rt, hold);

        // Сервер видит кадр, а событие об уходе - следующим.
        assert!(matches!(sent, SendAction::Done));
        assert_eq!(server.get(), Some(RendezvousOutcome::Delivered));
        assert!(
            matches!(ep.recv(&transport(BASE_B), &rt, None), RecvAction::SenderGone { badge } if badge == BADGE_A)
        );
    }

    #[test]
    fn an_in_flight_send_queues_no_frame_past_its_badge_departure() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);

        let hold = client.hold_for_operation();
        drop(client);

        // Получателя нет: кадр встаёт в очередь, и уход значка снимает его оттуда.
        let SendAction::Park {
            outcome: sender, ..
        } = ep.send(&transport(BASE_A).with_badge(BADGE_A), &rt, hold)
        else {
            panic!("send must park");
        };
        assert_eq!(sender.get(), Some(RendezvousOutcome::PeerGone));

        // Серверу достаётся событие об уходе, кадра снесённой сессии за ним нет.
        assert!(
            matches!(ep.recv(&transport(BASE_B), &rt, None), RecvAction::SenderGone { badge } if badge == BADGE_A)
        );
        assert!(matches!(
            ep.recv(&transport(BASE_B), &rt, None),
            RecvAction::Park { .. }
        ));
    }

    #[test]
    fn an_in_flight_call_delivers_its_frame_before_its_badge_departs() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);
        let RecvAction::Park {
            outcome: server,
            reply_slot,
            ..
        } = ep.recv(&transport(BASE_B), &rt, None)
        else {
            panic!("recv must park");
        };

        let hold = client.hold_for_operation();
        drop(client);

        let called = ep.call(
            &transport(BASE_A).with_badge(BADGE_A),
            ParkCtx::for_current(&rt),
            hold,
        );

        assert!(matches!(called, CallAction::Park { .. }));
        assert_eq!(server.get(), Some(RendezvousOutcome::Delivered));
        assert!(reply_slot.take().is_some());
    }

    #[test]
    fn port_recv_publishes_departed_badge_into_buffer() {
        use memory::memory_mapper::MemoryMapper;

        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        drop(badged_capability(&root, BADGE_A));

        let mapper = PageMapper::new(BASE_B);
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let receiver = ThreadTransport::new(mapper.clone(), VirtualAddress::new(BASE_B), table);

        assert_eq!(
            port_recv(&ep, &receiver, &rt, Some(0), None).map(|_| ()),
            Err(IpcError::SenderGone)
        );

        let mut raw = [0u8; 8];
        let badge_offset = core::mem::offset_of!(syscall::IpcBuffer, badge);
        mapper
            .copy_user_in(VirtualAddress::new(BASE_B + badge_offset), &mut raw)
            .expect("read badge back");
        assert_eq!(u64::from_le_bytes(raw), BADGE_A);
    }
}
