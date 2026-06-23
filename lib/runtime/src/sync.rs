//! Userspace `Mutex`/`Condvar` поверх Signal-syscall.
//!
//! Mutex - адаптированный 3-state futex: state 0=free, 1=locked без
//! waiter'ов, 2=locked с возможными waiter'ами. park/notify делегируются
//! Signal через `signal_wait_one`/`signal_set`; Signal создаётся лениво при
//! первой контенции.
//!
//! Уничтожать `Mutex`/`Condvar` только после завершения всех способных
//! заблокироваться на нём потоков: `Drop` закрывает Signal, на котором они паркуются.

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{
        AtomicU32,
        Ordering::{AcqRel, Acquire, Relaxed, Release},
    },
};

use syscall::{Handle, SIGNALED, WakeCount};

use crate::{handle_close, signal_create, signal_set, signal_wait_one};

/// Таймаут бесконечного паркинга.
const FOREVER: u64 = u64::MAX;

/// Взаимоисключающий доступ к `T` для userspace-потоков.
pub struct Mutex<T> {
    state: AtomicU32,
    signal_handle: AtomicU32,
    data: UnsafeCell<T>,
}

/// RAII-владение залоченным `Mutex`: разблокирует при `Drop`.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

/// Уведомление потоков, ждущих изменения состояния под `Mutex`.
pub struct Condvar {
    signal_handle: AtomicU32,
}

// SAFETY: Mutex сериализует доступ к `data`, поэтому при `T: Send` его можно
// передавать и шарить между потоками.
unsafe impl<T: Send> Send for Mutex<T> {}
// SAFETY: см. impl Send - эксклюзивность доступа гарантирует lock().
unsafe impl<T: Send> Sync for Mutex<T> {}

/// Лениво получает Signal по сырому handle из `ev`, создавая его при
/// первой контенции; гонку на создание разрешает CAS, лишний закрывается.
fn lazy_signal(signal_handle: &AtomicU32) -> Handle {
    let h = signal_handle.load(Acquire);
    if h != 0 {
        return Handle::new(h).unwrap();
    }

    // signal_create отказывает лишь при исчерпании ресурсов ядра; без Signal
    // парк невозможен, восстановиться нельзя (panic = abort по профилю).
    let new = signal_create().expect("signal_create");

    match signal_handle.compare_exchange(0, new.raw(), AcqRel, Acquire) {
        Ok(_) => new,
        Err(existing) => {
            handle_close(new);
            Handle::new(existing).unwrap()
        }
    }
}

impl<T> Mutex<T> {
    /// Создаёт открытый `Mutex` со значением `value`.
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicU32::new(0),
            signal_handle: AtomicU32::new(0),
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

    fn signal(&self) -> Handle {
        lazy_signal(&self.signal_handle)
    }

    /// Медленный путь: помечает lock как "с waiter'ами" (state=2) и паркуется
    /// на Signal, пока чей-то unlock не освободит state.
    fn lock_contended(&self) {
        loop {
            if self.state.swap(2, Acquire) == 0 {
                return;
            }
            let _ = signal_wait_one(self.signal(), SIGNALED, FOREVER);
            let _ = signal_set(self.signal(), 0, SIGNALED, WakeCount::None);
        }
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Drop for Mutex<T> {
    fn drop(&mut self) {
        let h = self.signal_handle.load(Relaxed);
        if h != 0 {
            handle_close(Handle::new(h).unwrap());
        }
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: удержание guard'а - эксклюзивный доступ к Mutex, поэтому
        // других живых ссылок на data нет.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: удержание guard'а - эксклюзивный доступ к Mutex, поэтому
        // других живых ссылок на data нет.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        if self.mutex.state.swap(0, Release) == 2 {
            let _ = signal_set(self.mutex.signal(), SIGNALED, 0, WakeCount::One);
        }
    }
}

impl Condvar {
    /// Создаёт `Condvar` без отложенных уведомлений.
    pub const fn new() -> Self {
        Self {
            signal_handle: AtomicU32::new(0),
        }
    }

    fn signal(&self) -> Handle {
        lazy_signal(&self.signal_handle)
    }

    /// Атомарно отпускает `guard`, паркуется до уведомления и перезахватывает
    /// мьютекс. Вызывать **только** в цикле `while !predicate { guard = cv.wait(guard) }`
    /// (возможны spurious-wakeup'ы; notify обязан сопровождать смену предиката
    /// под мьютексом).
    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        let mutex: &Mutex<T> = guard.mutex;
        drop(guard);
        let _ = signal_wait_one(self.signal(), SIGNALED, FOREVER);
        let _ = signal_set(self.signal(), 0, SIGNALED, WakeCount::None);
        mutex.lock()
    }

    /// Будит одного из waiter'ов, припаркованных на момент вызова.
    pub fn notify_one(&self) {
        let _ = signal_set(self.signal(), SIGNALED, 0, WakeCount::One);
    }

    /// Будит всех waiter'ов, припаркованных на момент вызова (см. контракт wait).
    pub fn notify_all(&self) {
        let _ = signal_set(self.signal(), SIGNALED, 0, WakeCount::All);
    }
}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Condvar {
    fn drop(&mut self) {
        let h = self.signal_handle.load(Relaxed);
        if h != 0 {
            handle_close(Handle::new(h).unwrap());
        }
    }
}
