use alloc::{collections::VecDeque, sync::Arc};
use core::{
    future::poll_fn,
    task::{Poll, Waker},
};

use spin::Mutex;

/// Концы канала из [`channel`].
pub struct Channel<T> {
    pub sender: Sender<T>,
    pub receiver: Receiver<T>,
}

/// Отправляющий конец канала из [`channel`].
pub struct Sender<T> {
    inner: Arc<Mutex<State<T>>>,
}

/// Принимающий конец канала из [`channel`]. При разрушении освобождает непрочитанные значения.
pub struct Receiver<T> {
    inner: Arc<Mutex<State<T>>>,
}

struct State<T> {
    queue: VecDeque<T>,
    waker: Option<Waker>,
    sender_alive: bool,
    receiver_alive: bool,
}

impl<T: Send> Sender<T> {
    /// Ставит `value` в неограниченную очередь и будит задачу, ждущую приёма.
    /// Отправка не ждёт приёма значения. Если приёмник разрушен, возвращает `value`
    /// отправителю.
    pub fn send(&self, value: T) -> Result<(), T> {
        let waker = {
            let mut state = self.inner.lock();
            if !state.receiver_alive {
                return Err(value);
            }
            state.queue.push_back(value);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }
}

impl<T> Receiver<T> {
    /// Ждёт следующее значение в порядке отправки. `None`, когда отправитель разрушен и
    /// очередь пуста.
    pub async fn recv(&mut self) -> Option<T> {
        poll_fn(|cx| {
            let mut state = self.inner.lock();
            if let Some(value) = state.queue.pop_front() {
                return Poll::Ready(Some(value));
            }
            if !state.sender_alive {
                return Poll::Ready(None);
            }
            match &mut state.waker {
                Some(waker) => waker.clone_from(cx.waker()),
                waker @ None => *waker = Some(cx.waker().clone()),
            }
            Poll::Pending
        })
        .await
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let waker = {
            let mut state = self.inner.lock();
            state.sender_alive = false;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        // Деструктор значения может обращаться к ядру, поэтому значения разрушаются вне лока.
        let queue = {
            let mut state = self.inner.lock();
            state.receiver_alive = false;
            state.waker = None;
            core::mem::take(&mut state.queue)
        };
        drop(queue);
    }
}

/// Создаёт канал, которым поток передаёт владеющие значения задаче [`Executor`](crate::Executor).
#[must_use]
pub fn channel<T>() -> Channel<T> {
    let inner = Arc::new(Mutex::new(State {
        queue: VecDeque::new(),
        waker: None,
        sender_alive: true,
        receiver_alive: true,
    }));
    Channel {
        sender: Sender {
            inner: Arc::clone(&inner),
        },
        receiver: Receiver { inner },
    }
}
