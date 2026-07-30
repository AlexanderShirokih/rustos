//! Состояние порта под его локом: очередь ждущих сторон и учёт держателей.
//!
//! Очередь держит либо отправителей, либо получателей - но никогда оба
//! одновременно. Учёт значков и получателей меняется только под тем же локом,
//! поэтому изменение множества и разбор очереди атомарны. Пробуждения наружу
//! выносятся значениями [`PendingWake`]: под локом порта они не выполняются.

use alloc::{collections::VecDeque, sync::Arc, vec::Vec};

use super::{
    outcome::{OutcomeSlot, RendezvousOutcome, SendKind},
    transport::ThreadTransport,
};
use crate::{
    errors::IpcError,
    handle_table::HandleTable,
    reply::ReplySlot,
    wait::{CancelTarget, ParkWaker},
};

// K - роль стороны: SendKind в очереди отправителей и () в очереди получателей.
pub(super) struct Waiter<K> {
    pub(super) kind: K,
    pub(super) transport: ThreadTransport,
    pub(super) waker: Arc<ParkWaker>,
    pub(super) outcome: Arc<OutcomeSlot>,
    pub(super) reply_slot: Arc<ReplySlot>,
}

pub(super) type SenderWaiter = Waiter<SendKind>;
pub(super) type ReceiverWaiter = Waiter<()>;

/// Очередь ожидания: либо отправители/вызыватели, либо получатели, но не оба.
enum Queue {
    Empty,
    Senders(VecDeque<SenderWaiter>),
    Receivers(VecDeque<ReceiverWaiter>),
}

/// Учётная запись держателей одного значка; живёт только под локом порта.
struct BadgeRecord {
    badge: u64,
    holders: u32,
}

/// Снятый с очереди waiter любой из двух ролей: под локом порта не роняется.
pub(super) enum QueuedWaiter {
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
pub(super) enum PendingWake {
    /// Событие доставлено: значок ушедшей стороны лежит в слоте исхода.
    Delivered(ReceiverWaiter),
    /// Встречи не будет.
    Broken(QueuedWaiter),
}

impl PendingWake {
    pub(super) fn wake(self) {
        match self {
            Self::Delivered(waiter) => waiter.waker.signal_match(),
            Self::Broken(waiter) => waiter.waker().cancel(),
        }
    }
}

/// Что достаётся получателю из состояния порта.
pub(super) enum RecvPick {
    /// Ушедший значок с непрочтённым событием.
    DepartedBadge(u64),
    /// Встречный отправитель или вызыватель, снятый с очереди.
    Sender(SenderWaiter),
    /// Ни события, ни отправителя.
    Nothing,
}

// Потолок непрочтённых событий об уходе: на нём порт не заводит записей под новые значки, а `recv`
// события освобождает запись.
pub(super) const DEPARTED_BADGE_LIMIT: usize = HandleTable::DEFAULT_CAPACITY as usize;

/// Очередь ожидания и учёт держателей под одним локом: `receivers` - живые capability с правом
/// `READ`, `badges` - записи значков, `departed` - записи с непрочтённым событием об уходе.
pub(super) struct PortState {
    queue: Queue,
    receivers: usize,
    badges: Vec<BadgeRecord>,
    departed: usize,
}

impl PortState {
    pub(super) fn new() -> Self {
        Self {
            queue: Queue::Empty,
            receivers: 0,
            badges: Vec::new(),
            departed: 0,
        }
    }

    /// Вводит нового получателя в множество.
    pub(super) fn add_receiver(&mut self) {
        self.receivers += 1;
    }

    #[cfg(test)]
    pub(super) fn queue_is_empty(&self) -> bool {
        matches!(self.queue, Queue::Empty)
    }

    // Оживление записи с непрочтённым событием проходит и на потолке: потолком ограничены только
    // новые записи.
    pub(super) fn register_badge(&mut self, badge: u64) -> Result<(), IpcError> {
        if let Some(record) = self.badges.iter_mut().find(|r| r.badge == badge) {
            if record.holders == 0 {
                self.departed -= 1;
            }
            record.holders += 1;
            return Ok(());
        }
        if self.departed >= DEPARTED_BADGE_LIMIT {
            return Err(IpcError::ResourceExhausted);
        }
        self.badges.push(BadgeRecord { badge, holders: 1 });
        Ok(())
    }

    pub(super) fn release_badge(&mut self, badge: u64, wakes: &mut Vec<PendingWake>) {
        let Some(index) = self.badges.iter().position(|r| r.badge == badge) else {
            debug_assert!(false, "release of an unregistered badge");
            return;
        };
        let record = &mut self.badges[index];
        record.holders = record.holders.saturating_sub(1);
        if record.holders > 0 {
            return;
        }
        for waiter in self.take_senders_with_badge(badge) {
            waiter.outcome.set(RendezvousOutcome::PeerGone);
            wakes.push(PendingWake::Broken(QueuedWaiter::Sender(waiter)));
        }
        match self.pop_receiver() {
            Some(receiver) => {
                receiver.outcome.set(RendezvousOutcome::SenderGone(badge));
                wakes.push(PendingWake::Delivered(receiver));
                self.badges.swap_remove(index);
            }
            None => self.departed += 1,
        }
    }

    pub(super) fn release_receiver(&mut self, wakes: &mut Vec<PendingWake>) {
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
        let index = self.badges.iter().position(|r| r.holders == 0);
        debug_assert!(
            index.is_some(),
            "departed counts badge records without holders"
        );
        let index = index?;
        self.departed -= 1;
        Some(self.badges.swap_remove(index).badge)
    }

    pub(super) fn take_receiver(&mut self) -> Result<Option<ReceiverWaiter>, IpcError> {
        if self.receivers == 0 {
            return Err(IpcError::PeerClosed);
        }
        Ok(self.pop_receiver())
    }

    // Готовое событие об уходе идёт впереди очереди отправителей.
    pub(super) fn pick_for_receiver(&mut self) -> RecvPick {
        if let Some(badge) = self.take_departed_badge() {
            return RecvPick::DepartedBadge(badge);
        }
        match self.pop_sender() {
            Some(sender) => RecvPick::Sender(sender),
            None => RecvPick::Nothing,
        }
    }

    pub(super) fn break_all(&mut self, wakes: &mut Vec<PendingWake>) {
        match core::mem::replace(&mut self.queue, Queue::Empty) {
            Queue::Empty => {}
            Queue::Senders(senders) => break_each(senders, QueuedWaiter::Sender, wakes),
            Queue::Receivers(receivers) => break_each(receivers, QueuedWaiter::Receiver, wakes),
        }
    }

    // Полный оборот деки на месте: совпавшие изымаются, порядок остальных сохраняется.
    fn take_senders_with_badge(&mut self, badge: u64) -> Vec<SenderWaiter> {
        let Queue::Senders(queued) = &mut self.queue else {
            return Vec::new();
        };
        let mut taken = Vec::new();
        for _ in 0..queued.len() {
            let Some(waiter) = queued.pop_front() else {
                break;
            };
            if waiter.transport.badge == badge {
                taken.push(waiter);
            } else {
                queued.push_back(waiter);
            }
        }
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
    pub(super) fn try_push_sender(&mut self, waiter: SenderWaiter) -> Result<(), SenderWaiter> {
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
    pub(super) fn try_push_receiver(
        &mut self,
        waiter: ReceiverWaiter,
    ) -> Result<(), ReceiverWaiter> {
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

    pub(super) fn take_by_waker(&mut self, waker: &Arc<ParkWaker>) -> Option<QueuedWaiter> {
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
