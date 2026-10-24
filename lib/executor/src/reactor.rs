use alloc::{
    boxed::Box,
    collections::{BTreeMap, btree_map::OccupiedEntry},
    rc::Rc,
    vec::Vec,
};
use core::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    sync::atomic::Ordering::SeqCst,
    task::{Context, Poll, Waker},
};

use kernel_objects::{Error, wait_any};
use syscall::{SIGNALED, SyscallError, Timeout, WAIT_MANY_MAX_COUNT, WaitItem, WakeCount};
use syscall_user::clock::Instant;

use crate::executor::Shared;

const REGISTRATIONS_MAX: usize = WAIT_MANY_MAX_COUNT - 1;

/// Ожидание объектов ядра задачами одного [`Executor`](crate::Executor).
#[derive(Clone)]
pub struct Reactor(Rc<ReactorState>);

/// Future из [`Reactor::wait`]. Разрушение снимает ожидание.
#[must_use = "future does nothing until awaited"]
pub struct Wait {
    reactor: Rc<ReactorState>,
    item: WaitItem,
    record: Option<usize>,
}

/// Future из [`Reactor::sleep`]. Разрушение снимает срок.
#[must_use = "future does nothing until awaited"]
pub struct Sleep {
    reactor: Rc<ReactorState>,
    deadline: Option<Instant>,
    timer: Option<u64>,
}

/// Future из [`Reactor::timeout`]. Возвращает `Some(result)`, если операция завершилась до
/// истечения срока, иначе - `None`. По истечении срока операция разрушается.
#[must_use = "future does nothing until awaited"]
pub struct Timed<F: Future> {
    future: Option<Pin<Box<F>>>,
    sleep: Sleep,
}

type TimerKey = (Instant, u64);

struct ReactorState {
    records: RefCell<Vec<Option<Registration>>>,
    start: Cell<usize>,
    timers: RefCell<BTreeMap<TimerKey, Waker>>,
    next_timer: Cell<u64>,
}

struct Registration {
    item: WaitItem,
    waker: Waker,
    outcome: Option<kernel_objects::Result<()>>,
}

enum Slot {
    Notify,
    Record(usize),
}

impl Reactor {
    pub(crate) fn new() -> Self {
        Self(Rc::new(ReactorState {
            records: RefCell::new(Vec::new()),
            start: Cell::new(0),
            timers: RefCell::new(BTreeMap::new()),
            next_timer: Cell::new(0),
        }))
    }

    /// Готов, когда на объекте `item` поднят бит его маски. Ожидание заканчивается ошибкой,
    /// если ядро отвергает либо отменяет его на этом handle. Ожидание сверх предела
    /// `SignalWaitMany` за вычетом записи пробуждения executor получает `ResourceExhausted`.
    pub fn wait(&self, item: WaitItem) -> Wait {
        Wait {
            reactor: Rc::clone(&self.0),
            item,
            record: None,
        }
    }

    /// Готов по истечении срока `after`. С [`Timeout::INFINITE`] future не готов никогда. Сроки
    /// не входят в предел ожиданий [`Self::wait`].
    pub fn sleep(&self, after: Timeout) -> Sleep {
        Sleep {
            reactor: Rc::clone(&self.0),
            deadline: Instant::now().after(after),
            timer: None,
        }
    }

    /// Ограничивает операцию `future` сроком `after`.
    pub fn timeout<F: Future>(&self, after: Timeout, future: F) -> Timed<F> {
        Timed {
            future: Some(Box::pin(future)),
            sleep: self.sleep(after),
        }
    }

    pub(crate) fn turn(&self, shared: &Shared, wait: Timeout) {
        let now = Instant::now();
        let wait = if self.0.wake_expired(now) {
            Timeout::POLL
        } else {
            wait.min(self.0.until_nearest(now))
        };

        let (slots, items) = self
            .0
            .pending(WaitItem::new(shared.notify.as_raw(), SIGNALED));
        if wait == Timeout::POLL && slots.len() == 1 {
            return;
        }

        let woken = wait_any(&items, wait);
        shared.sleeping.store(false, SeqCst);
        self.0.wake_expired(Instant::now());

        match woken {
            Ok(index) => match slots.get(index) {
                Some(Slot::Notify) => {
                    // Разбуженные задачи уже в очереди готовых, и снятие бита до её выборки
                    // не теряет пробуждение.
                    let _ = shared.notify.set(0, SIGNALED, WakeCount::None);
                }
                Some(&Slot::Record(record)) => {
                    self.0.start.set(record.saturating_add(1));
                    self.0.complete(record, Ok(()));
                }
                None => {}
            },
            Err(Error::Syscall(SyscallError::Timeout)) => {}
            // Отказ не называет запись, и опрос каждой записи находит готовые и отказавшие.
            Err(_) => self.0.probe(&slots, &items),
        }
    }
}

impl ReactorState {
    fn wake_expired(&self, now: Instant) -> bool {
        let mut woken = false;
        loop {
            let expired = self
                .timers
                .borrow_mut()
                .first_entry()
                .filter(|timer| timer.key().0 <= now)
                .map(OccupiedEntry::remove);
            let Some(waker) = expired else {
                return woken;
            };

            // `Waker` вызывается вне заёма таблицы сроков, и его код вправе снять срок.
            waker.wake();
            woken = true;
        }
    }

    fn until_nearest(&self, now: Instant) -> Timeout {
        self.timers
            .borrow()
            .first_key_value()
            .map_or(Timeout::INFINITE, |(&(deadline, _), _)| deadline.since(now))
    }

    fn pending(&self, notify: WaitItem) -> (Vec<Slot>, Vec<WaitItem>) {
        let records = self.records.borrow();
        let len = records.len();
        let start = self.start.get().checked_rem(len).unwrap_or(0);

        let mut slots = Vec::with_capacity(len.saturating_add(1));
        let mut items = Vec::with_capacity(len.saturating_add(1));
        // Среди готовых записей ядро выбирает первую, поэтому записи задач идут по кругу от
        // `start`, а `notify` идёт последней.
        let order = (start..len).chain(0..start);
        for record in order {
            if let Some(Some(registration)) = records.get(record)
                && registration.outcome.is_none()
            {
                slots.push(Slot::Record(record));
                items.push(registration.item);
            }
        }
        slots.push(Slot::Notify);
        items.push(notify);
        (slots, items)
    }

    fn probe(&self, slots: &[Slot], items: &[WaitItem]) {
        for (slot, item) in slots.iter().zip(items) {
            let &Slot::Record(record) = slot else {
                continue;
            };
            match wait_any(core::slice::from_ref(item), Timeout::POLL) {
                Ok(_) => self.complete(record, Ok(())),
                Err(Error::Syscall(SyscallError::Timeout)) => {}
                Err(error) => self.complete(record, Err(error)),
            }
        }
    }

    fn complete(&self, record: usize, outcome: kernel_objects::Result<()>) {
        let waker = {
            let mut records = self.records.borrow_mut();
            let Some(Some(registration)) = records.get_mut(record) else {
                return;
            };
            registration.outcome = Some(outcome);
            registration.waker.clone()
        };
        waker.wake();
    }

    fn register(&self, item: WaitItem, waker: &Waker) -> Option<usize> {
        let mut records = self.records.borrow_mut();
        if records.iter().flatten().count() >= REGISTRATIONS_MAX {
            return None;
        }
        let registration = Registration {
            item,
            waker: waker.clone(),
            outcome: None,
        };
        if let Some((record, free)) = records.iter_mut().enumerate().find(|(_, r)| r.is_none()) {
            *free = Some(registration);
            return Some(record);
        }
        records.push(Some(registration));
        Some(records.len().saturating_sub(1))
    }

    fn release(&self, record: usize) {
        let mut records = self.records.borrow_mut();
        if let Some(entry) = records.get_mut(record) {
            *entry = None;
        }
        // Места остальных записей не сдвигаются, и `start` вращает по ним.
        while records.last().is_some_and(Option::is_none) {
            records.pop();
        }
    }
}

impl Future for Wait {
    type Output = kernel_objects::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(record) = self.record else {
            let Some(record) = self.reactor.register(self.item, cx.waker()) else {
                return Poll::Ready(Err(Error::Syscall(SyscallError::ResourceExhausted)));
            };
            self.record = Some(record);
            return Poll::Pending;
        };

        let outcome = {
            let mut records = self.reactor.records.borrow_mut();
            let Some(Some(registration)) = records.get_mut(record) else {
                return Poll::Pending;
            };
            let Some(outcome) = registration.outcome.take() else {
                registration.waker.clone_from(cx.waker());
                return Poll::Pending;
            };
            outcome
        };
        self.reactor.release(record);
        self.record = None;
        Poll::Ready(outcome)
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let Some(deadline) = self.deadline else {
            return Poll::Pending;
        };

        if deadline <= Instant::now() {
            if let Some(id) = self.timer.take() {
                self.reactor.timers.borrow_mut().remove(&(deadline, id));
            }
            return Poll::Ready(());
        }

        let mut timers = self.reactor.timers.borrow_mut();
        if let Some(waker) = self.timer.and_then(|id| timers.get_mut(&(deadline, id))) {
            waker.clone_from(cx.waker());
            return Poll::Pending;
        }

        let id = self.reactor.next_timer.get();
        self.reactor.next_timer.set(id.wrapping_add(1));
        timers.insert((deadline, id), cx.waker().clone());
        drop(timers);

        self.timer = Some(id);
        Poll::Pending
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        if let (Some(deadline), Some(id)) = (self.deadline, self.timer) {
            self.reactor.timers.borrow_mut().remove(&(deadline, id));
        }
    }
}

impl<F: Future> Future for Timed<F> {
    type Output = Option<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        let output = match this.future.as_mut().map(|future| future.as_mut().poll(cx)) {
            None => None,
            Some(Poll::Ready(output)) => Some(output),
            Some(Poll::Pending) if Pin::new(&mut this.sleep).poll(cx).is_ready() => None,
            Some(Poll::Pending) => return Poll::Pending,
        };
        this.future = None;
        Poll::Ready(output)
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        if let Some(record) = self.record {
            self.reactor.release(record);
        }
    }
}
