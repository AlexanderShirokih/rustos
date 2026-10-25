use alloc::vec::Vec;
use core::{
    cell::{RefCell, RefMut},
    ops::{Deref, DerefMut},
    task::{Poll, Waker},
};

/// Исключительный доступ задач одного потока к значению, в том числе через `.await`.
/// Ожидание захвата уступает поток. Порядок захвата конкурирующими задачами не гарантирован.
pub struct Mutex<T> {
    value: RefCell<T>,
    waiters: RefCell<Vec<Option<Waker>>>,
}

/// Удерживает значение до разрушения и снимает заём перед пробуждением конкурентов.
#[must_use = "dropping the guard unlocks the mutex"]
pub struct MutexGuard<'a, T> {
    value: RefMut<'a, T>,
    _release: Release<'a>,
}

struct Release<'a>(&'a RefCell<Vec<Option<Waker>>>);

struct LockWait<'a> {
    waiters: &'a RefCell<Vec<Option<Waker>>>,
    slot: Option<usize>,
}

impl<T> Mutex<T> {
    /// Создаёт свободный mutex со значением `value`.
    pub const fn new(value: T) -> Self {
        Self {
            value: RefCell::new(value),
            waiters: RefCell::new(Vec::new()),
        }
    }

    /// Ждёт освобождения значения. Отмена future снимает ожидание захвата.
    pub async fn lock(&self) -> MutexGuard<'_, T> {
        let mut wait = LockWait {
            waiters: &self.waiters,
            slot: None,
        };

        core::future::poll_fn(|cx| {
            if let Ok(value) = self.value.try_borrow_mut() {
                return Poll::Ready(MutexGuard {
                    value,
                    _release: Release(&self.waiters),
                });
            }

            wait.register(cx.waker());
            Poll::Pending
        })
        .await
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl Drop for Release<'_> {
    fn drop(&mut self) {
        let waiting: Vec<_> = self.0.borrow().iter().flatten().cloned().collect();

        for waker in waiting {
            waker.wake();
        }
    }
}

impl LockWait<'_> {
    fn register(&mut self, waker: &Waker) {
        let mut waiters = self.waiters.borrow_mut();
        if let Some(Some(current)) = self.slot.and_then(|slot| waiters.get_mut(slot)) {
            current.clone_from(waker);
            return;
        }

        if let Some((slot, free)) = waiters.iter_mut().enumerate().find(|(_, w)| w.is_none()) {
            *free = Some(waker.clone());
            self.slot = Some(slot);
        } else {
            self.slot = Some(waiters.len());
            waiters.push(Some(waker.clone()));
        }
    }
}

impl Drop for LockWait<'_> {
    fn drop(&mut self) {
        let mut waiters = self.waiters.borrow_mut();
        if let Some(entry) = self.slot.and_then(|slot| waiters.get_mut(slot)) {
            *entry = None;
        }

        while waiters.last().is_some_and(Option::is_none) {
            waiters.pop();
        }
    }
}
