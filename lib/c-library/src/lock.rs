//! Замок с владельцем [`Lock`] и замки библиотеки C: retarget-функции `__retarget_lock_*` и
//! общий рекурсивный замок `__lock___libc_recursive_mutex`.
//!
//! `Lock` - мьютекс `runtime::RawMutex`, владелец и глубина вложенности. Он же `struct __lock`
//! и основа `pthread_mutex_t`. Владелец - адрес записи потока, нулевая структура - свободный
//! замок. При нехватке памяти инициализация выдаёт общий статический замок, и закрытие его
//! не освобождает.

#![allow(unsafe_code)]

use alloc::boxed::Box;
use core::{
    ptr,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed},
};

use runtime::RawMutex;

use crate::{process, thread};

#[unsafe(no_mangle)]
static __lock___libc_recursive_mutex: Lock = Lock::new();

#[repr(C)]
pub(crate) struct Lock {
    mutex: RawMutex,
    owner: AtomicUsize,
    depth: AtomicU32,
}

impl Lock {
    pub(crate) const fn new() -> Self {
        Self {
            mutex: RawMutex::new(),
            owner: AtomicUsize::new(0),
            depth: AtomicU32::new(0),
        }
    }

    pub(crate) fn acquire(&self) {
        self.mutex.lock();
        self.take(1);
    }

    pub(crate) fn try_acquire(&self) -> bool {
        let acquired = self.mutex.try_lock();

        if acquired {
            self.take(1);
        }

        acquired
    }

    pub(crate) fn reenter(&self) -> bool {
        let Some(depth) = self.depth.load(Relaxed).checked_add(1) else {
            return false;
        };

        self.depth.store(depth, Relaxed);
        true
    }

    pub(crate) fn owned(&self) -> bool {
        self.owner.load(Relaxed) == current_owner()
    }

    pub(crate) unsafe fn release(&self) {
        if self.depth.fetch_sub(1, Relaxed) > 1 {
            return;
        }

        self.owner.store(0, Relaxed);
        // SAFETY: вызывающий держит мьютекс по контракту функции, и это его последний уровень.
        unsafe { self.mutex.unlock() };
    }

    pub(crate) fn suspend(&self) -> u32 {
        self.owner.store(0, Relaxed);
        self.depth.swap(0, Relaxed)
    }

    pub(crate) fn resume(&self, depth: u32) {
        self.take(depth);
    }

    pub(crate) fn mutex(&self) -> &RawMutex {
        &self.mutex
    }

    fn take(&self, depth: u32) {
        self.owner.store(current_owner(), Relaxed);
        self.depth.store(depth, Relaxed);
    }

    fn acquire_recursive(&self) {
        if !self.owned() {
            self.acquire();
        } else if !self.reenter() {
            process::abort_with("recursive lock depth overflow");
        }
    }
}

fn current_owner() -> usize {
    ptr::from_ref(thread::current()).addr()
}

fn allocate() -> *const Lock {
    Box::try_new(Lock::new()).map_or(&raw const __lock___libc_recursive_mutex, |lock| {
        Box::into_raw(lock).cast_const()
    })
}

fn resolve(lock: *const Lock) -> &'static Lock {
    // SAFETY: библиотека C передаёт замок из `allocate` до его закрытия или общий статический.
    unsafe { lock.as_ref() }.unwrap_or(&__lock___libc_recursive_mutex)
}

fn close(lock: *const Lock) {
    if lock.is_null() || ptr::eq(lock, &raw const __lock___libc_recursive_mutex) {
        return;
    }

    // SAFETY: замок выдан `Box` в `allocate`, и библиотека C закрывает его один раз, когда
    // им больше никто не пользуется.
    drop(unsafe { Box::from_raw(lock.cast_mut()) });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __retarget_lock_init(lock: *mut *const Lock) {
    // SAFETY: `lock` указывает на `_LOCK_T` по контракту picolibc.
    unsafe { lock.write(allocate()) };
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __retarget_lock_init_recursive(lock: *mut *const Lock) {
    // SAFETY: `lock` указывает на `_LOCK_T` по контракту picolibc.
    unsafe { lock.write(allocate()) };
}

#[unsafe(no_mangle)]
extern "C" fn __retarget_lock_close(lock: *const Lock) {
    close(lock);
}

#[unsafe(no_mangle)]
extern "C" fn __retarget_lock_close_recursive(lock: *const Lock) {
    close(lock);
}

#[unsafe(no_mangle)]
extern "C" fn __retarget_lock_acquire(lock: *const Lock) {
    resolve(lock).mutex.lock();
}

#[unsafe(no_mangle)]
extern "C" fn __retarget_lock_acquire_recursive(lock: *const Lock) {
    resolve(lock).acquire_recursive();
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __retarget_lock_release(lock: *const Lock) {
    // SAFETY: библиотека C освобождает замок, захваченный этим потоком.
    unsafe { resolve(lock).mutex.unlock() };
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __retarget_lock_release_recursive(lock: *const Lock) {
    // SAFETY: библиотека C освобождает замок, захваченный этим потоком.
    unsafe { resolve(lock).release() };
}
