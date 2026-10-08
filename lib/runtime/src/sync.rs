//! Userspace `Mutex`/`Condvar` поверх futex.
//!
//! `RawMutex` - 3-state futex: state 0=free, 1=locked без waiter, 2=locked с
//! возможными waiter. Разбуженный поток перепроверяет `state` и на потерянном
//! пробуждении лишь повторяет круг. `Mutex<T>` защищает значение этим же словом.
//!
//! `RawCondvar` - счётчик уведомлений: waiter снимает его значение под мьютексом и
//! паркуется на нём же, notify меняет счётчик и будит по его адресу. Waiter,
//! пришедший после notify, паркуется на новом значении и чужого уведомления не
//! получает. `Condvar` ждёт через `RawCondvar` с `MutexGuard`.
//!
//! Уничтожать `Mutex`/`Condvar` нужно только после завершения всех способных
//! заблокироваться на нём потоков. Припаркованный поток ждёт по адресу слова
//! состояния и продолжает ждать даже после его освобождения. `RawCondvar` можно
//! уничтожить после [`RawCondvar::destroy`].

use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    mem,
    ops::{Deref, DerefMut},
    sync::atomic::{
        AtomicU32,
        Ordering::{Acquire, Relaxed, Release},
    },
};

use crate::futex::{futex_wait, futex_wake, futex_wake_all};

/// Взаимоисключение потоков на одном слове без защищаемого значения.
///
/// Захват и освобождение - отдельные вызовы одного потока.
pub struct RawMutex {
    state: AtomicU32,
}

/// Взаимоисключающий доступ к `T` для userspace-потоков.
pub struct Mutex<T> {
    raw: RawMutex,
    data: UnsafeCell<T>,
}

/// RAII-владение залоченным `Mutex`: разблокирует при `Drop`.
///
/// Guard содержит `PhantomData<&mut T>`, чтобы авто-трейты считались по эксклюзивному доступу:
/// guard `Sync` ровно при `T: Sync`. На одной ссылке `&Mutex<T>` они дают `Sync` уже при
/// `T: Send`, и тогда два потока достают из `MutexGuard<Cell<_>>` по `&Cell<_>` через `Deref` -
/// гонка в безопасном коде.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
    exclusive: PhantomData<&'a mut T>,
}

const DESTROYING: u32 = 1 << 31;

/// Уведомление потоков, ждущих изменения состояния под [`RawMutex`].
pub struct RawCondvar {
    notifications: AtomicU32,
    waiters: AtomicU32,
}

/// Уведомление потоков, ждущих изменения состояния под `Mutex`.
pub struct Condvar {
    raw: RawCondvar,
}

// SAFETY: Mutex сериализует доступ к `data`, поэтому при `T: Send` его можно
// передавать и шарить между потоками.
unsafe impl<T: Send> Send for Mutex<T> {}
// SAFETY: см. impl Send - эксклюзивность доступа гарантирует lock().
unsafe impl<T: Send> Sync for Mutex<T> {}

impl RawMutex {
    /// Создаёт свободный мьютекс.
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(0),
        }
    }

    /// Блокирует до захвата мьютекса вызывающим потоком.
    pub fn lock(&self) {
        if self.state.compare_exchange(0, 1, Acquire, Relaxed).is_ok() {
            return;
        }

        self.lock_contended();
    }

    /// Захватывает свободный мьютекс без ожидания. `false` - мьютекс занят.
    pub fn try_lock(&self) -> bool {
        self.state.compare_exchange(0, 1, Acquire, Relaxed).is_ok()
    }

    /// Освобождает мьютекс и будит одного ждущего.
    ///
    /// # Safety
    ///
    /// Вызывающий поток держит мьютекс, захваченный [`Self::lock`].
    pub unsafe fn unlock(&self) {
        if self.state.swap(0, Release) == 2 {
            futex_wake(&self.state);
        }
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

impl Default for RawMutex {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Mutex<T> {
    /// Создаёт открытый `Mutex` со значением `value`.
    pub const fn new(value: T) -> Self {
        Self {
            raw: RawMutex::new(),
            data: UnsafeCell::new(value),
        }
    }

    /// Блокирует до получения эксклюзивного доступа и отдаёт guard.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.raw.lock();
        MutexGuard {
            mutex: self,
            exclusive: PhantomData,
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
        // SAFETY: guard существует только после захвата `lock` и освобождает мьютекс один раз.
        unsafe { self.mutex.raw.unlock() };
    }
}

impl RawCondvar {
    /// Создаёт `RawCondvar` без отложенных уведомлений.
    pub const fn new() -> Self {
        Self {
            notifications: AtomicU32::new(0),
            waiters: AtomicU32::new(0),
        }
    }

    /// Атомарно освобождает `mutex`, паркуется до уведомления и захватывает
    /// `mutex` заново. Возможен возврат без уведомления.
    ///
    /// # Safety
    ///
    /// Вызывающий поток держит `mutex`, захваченный [`RawMutex::lock`].
    pub unsafe fn wait(&self, mutex: &RawMutex) {
        // SAFETY: контракт передан вызывающему.
        unsafe { self.wait_interleaved(mutex, || {}) }
    }

    /// Тестовый разрез [`Self::wait`]: `between` вызывается в окне между
    /// отпусканием мьютекса и парковкой - там, где waiter уже снял счётчик
    /// уведомлений, но ещё не спит.
    ///
    /// # Safety
    ///
    /// См. [`Self::wait`].
    #[doc(hidden)]
    pub unsafe fn wait_interleaved(&self, mutex: &RawMutex, between: impl FnOnce()) {
        self.waiters.fetch_add(1, Relaxed);
        // Счётчик снимается под мьютексом, под которым меняется и условие.
        let expected = self.notifications.load(Relaxed);

        // SAFETY: вызывающий держит `mutex` по контракту функции.
        unsafe { mutex.unlock() };
        between();
        futex_wait(&self.notifications, expected, None);

        // Последнее обращение к памяти переменной, после него `destroy` может её освободить.
        if self.waiters.fetch_sub(1, Release) == DESTROYING | 1 {
            futex_wake(&self.waiters);
        }

        mutex.lock();
    }

    /// Снимает с ожидания waiter, припаркованный на момент вызова.
    pub fn notify_one(&self) {
        self.notifications.fetch_add(1, Release);
        futex_wake(&self.notifications);
    }

    /// Снимает с ожидания всех waiter, припаркованных на момент вызова.
    pub fn notify_all(&self) {
        self.notifications.fetch_add(1, Release);
        futex_wake_all(&self.notifications);
    }

    /// Дожидается выхода уведомлённых waiter. После возврата память переменной
    /// можно освободить.
    ///
    /// Ни один поток не должен ждать на переменной без уведомления, иначе вызов
    /// не вернётся.
    pub fn destroy(&self) {
        let mut waiters = self.waiters.fetch_or(DESTROYING, Acquire) | DESTROYING;

        while waiters != DESTROYING {
            futex_wait(&self.waiters, waiters, None);
            waiters = self.waiters.load(Acquire);
        }
    }
}

impl Default for RawCondvar {
    fn default() -> Self {
        Self::new()
    }
}

impl Condvar {
    /// Создаёт `Condvar` без отложенных уведомлений.
    pub const fn new() -> Self {
        Self {
            raw: RawCondvar::new(),
        }
    }

    /// Атомарно отпускает `guard`, паркуется до уведомления и перезахватывает
    /// мьютекс. Вызывать **только** в цикле `while !predicate { guard = cv.wait(guard) }`
    /// (возможен spurious wakeup, предикат меняется под мьютексом).
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
        // Мьютекс освобождает ожидание вместо `Drop` у guard.
        mem::forget(guard);
        // SAFETY: guard удерживал `mutex.raw`.
        unsafe { self.raw.wait_interleaved(&mutex.raw, between) };

        MutexGuard {
            mutex,
            exclusive: PhantomData,
        }
    }

    /// Снимает с ожидания waiter, припаркованный на момент вызова (см. контракт
    /// [`Self::wait`]).
    pub fn notify_one(&self) {
        self.raw.notify_one();
    }

    /// Снимает с ожидания всех waiter, припаркованных на момент вызова (см.
    /// контракт [`Self::wait`]).
    pub fn notify_all(&self) {
        self.raw.notify_all();
    }
}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}
