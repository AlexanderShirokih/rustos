use alloc::{
    collections::BTreeMap,
    rc::{Rc, Weak},
};
use core::{
    any::Any,
    cell::RefCell,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker, ready},
};

use crate::executor::{Task, TaskId};

/// Ожидание результата задачи и её отмена. Разрушение handle не отменяет задачу.
///
/// Готов с `Some(output)` после завершения задачи и с `None`, если задача отменена либо её
/// executor освобождён до завершения.
pub struct JoinHandle<T> {
    slot: Rc<Slot<T>>,
    tasks: Weak<RefCell<BTreeMap<TaskId, Task>>>,
    id: TaskId,
}

/// Владелец задачи: [`JoinHandle`], разрушение которого отменяет незавершённую задачу.
#[must_use = "dropping the handle cancels its task"]
pub struct TaskHandle<T = ()>(JoinHandle<T>);

pub(crate) trait TaskFuture {
    fn poll_task(self: Pin<&mut Self>, cx: &mut Context<'_>, slot: &dyn Any) -> Poll<()>;
}

pub(crate) trait TaskSlot: Any {
    fn release(&self);

    fn is_pending(&self) -> bool;
}

pub(crate) struct Slot<T>(RefCell<State<T>>);

enum State<T> {
    Pending(Option<Waker>),
    Ready(T),
    Released,
}

impl<F: Future<Output: 'static>> TaskFuture for F {
    fn poll_task(self: Pin<&mut Self>, cx: &mut Context<'_>, slot: &dyn Any) -> Poll<()> {
        let output = ready!(self.poll(cx));
        if let Some(slot) = slot.downcast_ref::<Slot<F::Output>>() {
            slot.finish(output);
        }

        Poll::Ready(())
    }
}

impl<T> Slot<T> {
    pub(crate) const fn new() -> Self {
        Self(RefCell::new(State::Pending(None)))
    }

    fn finish(&self, output: T) {
        let previous = self.0.replace(State::Ready(output));
        if let State::Pending(Some(waker)) = previous {
            waker.wake();
        }
    }

    fn release(&self) {
        let waiting = {
            let mut state = self.0.borrow_mut();
            let State::Pending(waker) = &mut *state else {
                return;
            };

            let waiting = waker.take();
            *state = State::Released;
            waiting
        };

        if let Some(waker) = waiting {
            waker.wake();
        }
    }

    fn is_pending(&self) -> bool {
        matches!(*self.0.borrow(), State::Pending(_))
    }
}

impl<T: 'static> TaskSlot for Slot<T> {
    fn release(&self) {
        Self::release(self);
    }

    fn is_pending(&self) -> bool {
        Self::is_pending(self)
    }
}

impl<T> JoinHandle<T> {
    pub(crate) const fn new(
        slot: Rc<Slot<T>>,
        tasks: Weak<RefCell<BTreeMap<TaskId, Task>>>,
        id: TaskId,
    ) -> Self {
        Self { slot, tasks, id }
    }

    pub(crate) const fn id(&self) -> TaskId {
        self.id
    }

    /// Отменяет незавершённую задачу и освобождает её future.
    pub fn abort(&self) {
        self.slot.release();

        if let Some(tasks) = self.tasks.upgrade() {
            let task = tasks.borrow_mut().remove(&self.id);
            // Разрушение future может освободить владельцев других задач той же таблицы.
            drop(task);
        }
    }

    /// Задача завершилась, отменена либо её executor освобождён.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        !self.slot.is_pending()
    }

    /// Владелец, разрушение которого отменяет задачу.
    pub const fn abort_on_drop(self) -> TaskHandle<T> {
        TaskHandle(self)
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut state = self.slot.0.borrow_mut();
        if let State::Pending(waker) = &mut *state {
            *waker = Some(cx.waker().clone());
            return Poll::Pending;
        }

        match core::mem::replace(&mut *state, State::Released) {
            State::Ready(output) => Poll::Ready(Some(output)),
            State::Pending(_) | State::Released => Poll::Ready(None),
        }
    }
}

impl<T> TaskHandle<T> {
    /// Задача завершилась, отменена либо её executor освобождён.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

impl<T> Future for TaskHandle<T> {
    type Output = Option<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        Pin::new(&mut self.0).poll(cx)
    }
}

impl<T> Drop for TaskHandle<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
