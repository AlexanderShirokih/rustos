use alloc::{
    boxed::Box,
    collections::{BTreeMap, btree_map::OccupiedEntry},
    rc::Rc,
    vec::Vec,
};
use core::{
    cell::{Cell, RefCell},
    future::{Future, IntoFuture},
    pin::Pin,
    sync::atomic::Ordering::SeqCst,
    task::{Context, Poll, Waker},
};

use kernel_objects::Error;
use syscall::{SIGNALED, SyscallError, Timeout, WAIT_MANY_MAX_COUNT, WaitItem, WakeCount};
use syscall_user::clock::Instant;

use crate::executor::Shared;

const REGISTRATIONS_MAX: usize = WAIT_MANY_MAX_COUNT - 1;

#[thread_local]
static CURRENT: RefCell<Option<Rc<ReactorState>>> = RefCell::new(None);

pub(crate) struct Reactor(Rc<ReactorState>);

pub(crate) struct Entered(Option<Rc<ReactorState>>);

/// Future из [`wait`]. Разрушение снимает ожидание.
#[must_use = "future does nothing until awaited"]
pub struct Wait {
    registered: Option<(Rc<ReactorState>, usize)>,
    item: WaitItem,
}

/// Future из [`sleep`]. Разрушение снимает срок.
#[must_use = "future does nothing until awaited"]
pub struct Sleep {
    deadline: Option<Instant>,
    timer: Option<(Rc<ReactorState>, u64)>,
}

/// Future из [`timeout`]. Возвращает `Some(result)`, если операция завершилась до
/// истечения срока, иначе - `None`. По истечении срока операция разрушается.
#[must_use = "future does nothing until awaited"]
pub struct Timed<F: Future> {
    future: Option<Pin<Box<F>>>,
    sleep: Sleep,
}

type TimerKey = (Instant, u64);

struct ReactorState {
    records: RefCell<Vec<Option<Registration>>>,
    pending: RefCell<Pending>,
    start: Cell<usize>,
    timers: RefCell<BTreeMap<TimerKey, Waker>>,
    next_timer: Cell<u64>,
}

struct Registration {
    item: WaitItem,
    waker: Waker,
    outcome: Option<kernel_objects::Result<()>>,
}

#[derive(Default)]
struct Pending {
    slots: Vec<Slot>,
    items: Vec<WaitItem>,
}

enum Slot {
    Notify,
    Record(usize),
}

impl Reactor {
    pub(crate) fn new() -> Self {
        Self(Rc::new(ReactorState {
            records: RefCell::new(Vec::new()),
            pending: RefCell::new(Pending::default()),
            start: Cell::new(0),
            timers: RefCell::new(BTreeMap::new()),
            next_timer: Cell::new(0),
        }))
    }

    pub(crate) fn enter(&self) -> Entered {
        Entered(CURRENT.replace(Some(Rc::clone(&self.0))))
    }

    pub(crate) fn turn(&self, shared: &Shared, wait: Timeout) {
        let now = Instant::now();
        let wait = if self.0.wake_expired(now) {
            Timeout::POLL
        } else {
            wait.min(self.0.until_nearest(now))
        };

        let mut pending = self.0.pending.take();
        self.0.collect(
            &mut pending,
            WaitItem::new(shared.notify.as_raw(), SIGNALED),
        );
        if wait != Timeout::POLL || pending.slots.len() > 1 {
            self.wait_pending(shared, wait, &pending);
        }
        self.0.pending.replace(pending);
    }

    fn wait_pending(&self, shared: &Shared, wait: Timeout, pending: &Pending) {
        let Pending { slots, items } = pending;
        let woken = kernel_objects::wait_any(items, wait);
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
            Err(_) => self.0.probe(slots, items),
        }
    }
}

impl Drop for Entered {
    fn drop(&mut self) {
        CURRENT.replace(self.0.take());
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

    fn collect(&self, pending: &mut Pending, notify: WaitItem) {
        let records = self.records.borrow();
        let len = records.len();
        let start = self.start.get().checked_rem(len).unwrap_or(0);

        let Pending { slots, items } = pending;
        slots.clear();
        items.clear();
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
    }

    fn probe(&self, slots: &[Slot], items: &[WaitItem]) {
        for (slot, item) in slots.iter().zip(items) {
            let &Slot::Record(record) = slot else {
                continue;
            };
            match kernel_objects::wait_any(core::slice::from_ref(item), Timeout::POLL) {
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
        let Some((reactor, record)) = &self.registered else {
            let reactor = current();
            let Some(record) = reactor.register(self.item, cx.waker()) else {
                return Poll::Ready(Err(Error::Syscall(SyscallError::ResourceExhausted)));
            };
            self.registered = Some((reactor, record));

            return Poll::Pending;
        };

        let outcome = {
            let mut records = reactor.records.borrow_mut();
            let Some(Some(registration)) = records.get_mut(*record) else {
                return Poll::Pending;
            };
            let Some(outcome) = registration.outcome.take() else {
                registration.waker.clone_from(cx.waker());
                return Poll::Pending;
            };
            outcome
        };
        if let Some((reactor, record)) = self.registered.take() {
            reactor.release(record);
        }

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
            if let Some((reactor, id)) = self.timer.take() {
                reactor.timers.borrow_mut().remove(&(deadline, id));
            }
            return Poll::Ready(());
        }

        if let Some((reactor, id)) = &self.timer
            && let Some(waker) = reactor.timers.borrow_mut().get_mut(&(deadline, *id))
        {
            waker.clone_from(cx.waker());
            return Poll::Pending;
        }

        let reactor = current();
        let id = reactor.next_timer.get();
        reactor.next_timer.set(id.wrapping_add(1));
        reactor
            .timers
            .borrow_mut()
            .insert((deadline, id), cx.waker().clone());
        self.timer = Some((reactor, id));

        Poll::Pending
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        if let (Some(deadline), Some((reactor, id))) = (self.deadline, &self.timer) {
            reactor.timers.borrow_mut().remove(&(deadline, *id));
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
        if let Some((reactor, record)) = &self.registered {
            reactor.release(*record);
        }
    }
}

/// Готов, когда на объекте `item` поднят бит его маски. Первый опрос регистрирует ожидание в
/// текущем executor. Регистрация без текущего executor вызывает панику. Ожидание заканчивается
/// ошибкой, если ядро отвергает либо отменяет его на этом handle. Ожидание сверх предела
/// `SignalWaitMany` за вычетом записи пробуждения executor получает `ResourceExhausted`.
pub const fn wait(item: WaitItem) -> Wait {
    Wait {
        registered: None,
        item,
    }
}

/// Ждёт готовности одного из `items` и возвращает его индекс. Каждая запись входит в предел
/// [`wait`]. Пустой набор остаётся в ожидании до отмены future.
pub async fn wait_any(items: &[WaitItem]) -> kernel_objects::Result<usize> {
    let mut waits: Vec<_> = items.iter().map(|item| wait(*item)).collect();

    core::future::poll_fn(|cx| {
        for (index, wait) in waits.iter_mut().enumerate() {
            if let Poll::Ready(outcome) = Pin::new(wait).poll(cx) {
                return Poll::Ready(outcome.map(|()| index));
            }
        }

        Poll::Pending
    })
    .await
}

/// Готов по истечении срока `after`, отсчитанного от вызова. С [`Timeout::INFINITE`] остаётся
/// в ожидании до отмены future. Сроки не входят в предел ожиданий [`wait`].
pub fn sleep(after: Timeout) -> Sleep {
    Sleep {
        deadline: Instant::now().after(after),
        timer: None,
    }
}

/// Ограничивает операцию `future` сроком `after`.
pub fn timeout<F: IntoFuture>(after: Timeout, future: F) -> Timed<F::IntoFuture> {
    Timed {
        future: Some(Box::pin(future.into_future())),
        sleep: sleep(after),
    }
}

fn current() -> Rc<ReactorState> {
    let current = CURRENT.borrow().clone();
    // Регистрация требует executor, который проверяет готовность ожидания и будит задачу.
    #[allow(clippy::panic)]
    current.unwrap_or_else(|| panic!("executor wait polled outside an executor task"))
}
