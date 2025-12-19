use core::cell::UnsafeCell;

pub trait LockCell<T> {
    fn new(value: T) -> Self;
    fn into_inner(self) -> T;
    fn with_lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;
}

pub struct NoLockCell<T> {
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

pub struct MutexCell<T> {
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
