//! Взаимоисключение поверх futex для внутренних нужд крейта.
//!
//! `LockGuard` несёт `PhantomData<&mut T>`, чтобы авто-трейты считались по эксклюзивному
//! доступу: guard `Sync` ровно при `T: Sync`. На одной ссылке `&Lock<T>` они дают `Sync` уже
//! при `T: Send`, и тогда два потока достают из `LockGuard<Cell<_>>` по `&Cell<_>` через
//! `Deref` — гонка в безопасном коде.

use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{
        AtomicU32,
        Ordering::{Acquire, Relaxed, Release},
    },
};

use crate::futex::{Futex, futex_wait, futex_wake};

const FREE: u32 = 0;

const HELD: u32 = 1;

const CONTENDED: u32 = 2;

/// Взаимоисключающий доступ к `T`.
pub struct Lock<T> {
    state: Futex,
    value: UnsafeCell<T>,
}

/// Владение залоченным [`Lock`]. Отпускает его на drop.
pub struct LockGuard<'a, T> {
    lock: &'a Lock<T>,
    exclusive: PhantomData<&'a mut T>,
}

// SAFETY: доступ к `value` сериализован состоянием futex, поэтому `T: Send` достаточно.
unsafe impl<T: Send> Send for Lock<T> {}
// SAFETY: см. impl Send — эксклюзивность доступа гарантирует `lock`.
unsafe impl<T: Send> Sync for Lock<T> {}

impl<T> Lock<T> {
    /// Создаёт отпущенный `Lock` со значением `value`.
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicU32::new(FREE),
            value: UnsafeCell::new(value),
        }
    }

    /// Блокирует до получения эксклюзивного доступа.
    pub fn lock(&self) -> LockGuard<'_, T> {
        if self
            .state
            .compare_exchange(FREE, HELD, Acquire, Relaxed)
            .is_err()
        {
            self.lock_contended();
        }
        LockGuard {
            lock: self,
            exclusive: PhantomData,
        }
    }

    fn lock_contended(&self) {
        while self.state.swap(CONTENDED, Acquire) != FREE {
            futex_wait(&self.state, CONTENDED, None);
        }
    }
}

impl<T> Deref for LockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: пока guard жив, доступ эксклюзивен, других живых ссылок нет.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for LockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: пока guard жив, доступ эксклюзивен, других живых ссылок нет.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for LockGuard<'_, T> {
    fn drop(&mut self) {
        if self.lock.state.swap(FREE, Release) == CONTENDED {
            futex_wake(&self.lock.state);
        }
    }
}
