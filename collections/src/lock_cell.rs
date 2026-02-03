use core::cell::UnsafeCell;

/// Абстракция над ячейкой с эксклюзивным доступом.
///
/// Позволяет выбирать стратегию синхронизации: без блокировок
/// для однопоточного кода или со спинлоком для многопоточного.
pub trait LockCell<T> {
    /// Создаёт ячейку с начальным значением.
    fn new(value: T) -> Self;

    /// Извлекает значение, потребляя ячейку.
    fn into_inner(self) -> T;

    /// Выполняет замыкание с эксклюзивным доступом к значению.
    fn with_lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;
}

/// Ячейка без синхронизации для однопоточного кода.
///
/// # Safety
///
/// Вызывающий код должен гарантировать отсутствие
/// одновременного доступа из разных потоков.
pub struct NoLockCell<T> {
    /// Внутреннее значение.
    inner: UnsafeCell<T>,
}

impl<T> NoLockCell<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: UnsafeCell::new(value),
        }
    }
}

impl<T> LockCell<T> for NoLockCell<T> {
    fn new(value: T) -> Self {
        Self::new(value)
    }

    fn into_inner(self) -> T {
        self.inner.into_inner()
    }

    fn with_lock<R>(&self, lock: impl FnOnce(&mut T) -> R) -> R {
        let ptr = self.inner.get();

        // SAFETY: Вызывающий код гарантирует однопоточный доступ
        lock(unsafe { &mut *ptr })
    }
}

/// Ячейка с защитой через спинлок.
pub struct MutexCell<T> {
    /// Мьютекс для синхронизации доступа.
    mutex: spin::Mutex<T>,
}

impl<T> MutexCell<T> {
    pub const fn new(value: T) -> Self {
        Self {
            mutex: spin::Mutex::new(value),
        }
    }
}

impl<T> LockCell<T> for MutexCell<T> {
    fn new(value: T) -> Self {
        Self::new(value)
    }

    fn into_inner(self) -> T {
        self.mutex.into_inner()
    }

    fn with_lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.mutex.lock();
        f(&mut *guard)
    }
}
