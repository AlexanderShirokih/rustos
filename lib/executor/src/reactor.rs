use alloc::{rc::Rc, vec::Vec};
use core::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    sync::atomic::Ordering::SeqCst,
    task::{Context, Poll, Waker},
};

use kernel_objects::{Error, wait_any};
use syscall::{SIGNALED, SyscallError, Timeout, WAIT_MANY_MAX_COUNT, WaitItem, WakeCount};

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

struct ReactorState {
    records: RefCell<Vec<Option<Registration>>>,
    start: Cell<usize>,
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

    pub(crate) fn turn(&self, shared: &Shared, wait: Timeout) {
        let (slots, items) = self
            .0
            .pending(WaitItem::new(shared.notify.as_raw(), SIGNALED));
        if wait == Timeout::POLL && slots.len() == 1 {
            return;
        }
        let woken = wait_any(&items, wait);
        shared.sleeping.store(false, SeqCst);

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

impl Drop for Wait {
    fn drop(&mut self) {
        if let Some(record) = self.record {
            self.reactor.release(record);
        }
    }
}
