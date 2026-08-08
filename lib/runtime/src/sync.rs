//! Userspace `Mutex`/`Condvar` поверх futex.
//!
//! Mutex - 3-state futex: state 0=free, 1=locked без waiter, 2=locked с
//! возможными waiter. Разбуженный поток перепроверяет `state` и на потерянном
//! пробуждении лишь повторяет круг.
//!
//! Condvar - счётчик уведомлений: waiter снимает его значение под мьютексом и
//! паркуется на нём же, notify меняет счётчик и будит по его адресу. Waiter,
//! пришедший после notify, паркуется на новом значении и чужого уведомления не
//! получает.
//!
//! Уничтожать `Mutex`/`Condvar` только после завершения всех способных
//! заблокироваться на нём потоков: припаркованный поток ждёт по адресу слова
//! состояния и переживает его освобождение.

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{
        AtomicU32,
        Ordering::{Acquire, Relaxed, Release},
    },
};

use crate::futex::{futex_wait, futex_wake, futex_wake_all};

/// Взаимоисключающий доступ к `T` для userspace-потоков.
pub struct Mutex<T> {
    state: AtomicU32,
    data: UnsafeCell<T>,
}

/// RAII-владение залоченным `Mutex`: разблокирует при `Drop`.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

/// Уведомление потоков, ждущих изменения состояния под `Mutex`.
pub struct Condvar {
    notifications: AtomicU32,
}

// SAFETY: Mutex сериализует доступ к `data`, поэтому при `T: Send` его можно
// передавать и шарить между потоками.
unsafe impl<T: Send> Send for Mutex<T> {}
// SAFETY: см. impl Send - эксклюзивность доступа гарантирует lock().
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    /// Создаёт открытый `Mutex` со значением `value`.
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicU32::new(0),
            data: UnsafeCell::new(value),
        }
    }

    /// Блокирует до получения эксклюзивного доступа и отдаёт guard.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        if self.state.compare_exchange(0, 1, Acquire, Relaxed).is_ok() {
            return MutexGuard { mutex: self };
        }

        self.lock_contended();
        MutexGuard { mutex: self }
    }

    fn lock_contended(&self) {
        loop {
            if self.state.swap(2, Acquire) == 0 {
                return;
            }
            futex_wait(&self.state, 2, None);
        }
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: пока guard жив, доступ к Mutex эксклюзивен, поэтому других
        // живых ссылок на data нет.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: пока guard жив, доступ к Mutex эксклюзивен, поэтому других
        // живых ссылок на data нет.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        if self.mutex.state.swap(0, Release) == 2 {
            futex_wake(&self.mutex.state);
        }
    }
}

impl Condvar {
    /// Создаёт `Condvar` без отложенных уведомлений.
    pub const fn new() -> Self {
        Self {
            notifications: AtomicU32::new(0),
        }
    }

    /// Атомарно отпускает `guard`, паркуется до уведомления и перезахватывает
    /// мьютекс. Вызывать **только** в цикле `while !predicate { guard = cv.wait(guard) }`
    /// (возможен spurious wakeup, notify обязан сопровождать смену предиката
    /// под мьютексом).
    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        self.wait_interleaved(guard, || {})
    }

    /// Тестовый разрез [`Self::wait`]: `between` вызывается в окне между
    /// отпусканием мьютекса и парковкой - там, где waiter уже снял счётчик
    /// уведомлений, но ещё не спит.
    #[doc(hidden)]
    pub fn wait_interleaved<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        between: impl FnOnce(),
    ) -> MutexGuard<'a, T> {
        let mutex: &Mutex<T> = guard.mutex;
        // Счётчик снимается под мьютексом: notify меняет его только там же.
        let expected = self.notifications.load(Relaxed);

        drop(guard);
        between();

        futex_wait(&self.notifications, expected, None);
        mutex.lock()
    }

    /// Снимает с ожидания waiter, припаркованный на момент вызова (см. контракт
    /// [`Self::wait`]).
    pub fn notify_one(&self) {
        self.notifications.fetch_add(1, Relaxed);
        futex_wake(&self.notifications);
    }

    /// Снимает с ожидания всех waiter, припаркованных на момент вызова (см.
    /// контракт [`Self::wait`]).
    pub fn notify_all(&self) {
        self.notifications.fetch_add(1, Relaxed);
        futex_wake_all(&self.notifications);
    }
}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}
