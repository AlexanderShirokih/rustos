//! Блокировка чтения-записи POSIX `pthread_rwlock_t` поверх `runtime::Mutex` и
//! `runtime::Condvar`.
//!
//! Нулевая структура - свободная блокировка, её задаёт `PTHREAD_RWLOCK_INITIALIZER`. На чтение
//! блокировку держат одновременно несколько потоков, на запись - один. Захват ждёт на условной
//! переменной блокировки, пока держатели не освободят её.

#![allow(unsafe_code)]

use core::{
    ffi::{c_int, c_void},
    mem::MaybeUninit,
};

use runtime::{Condvar, Mutex};

use crate::errno::Errno;

const _: () = assert!(size_of::<RwLock>() == 20 && align_of::<RwLock>() == 4);

struct RwLock {
    holders: Mutex<Holders>,
    released: Condvar,
}

struct Holders {
    readers: u32,
    writer: bool,
}

impl RwLock {
    const fn new() -> Self {
        Self {
            holders: Mutex::new(Holders {
                readers: 0,
                writer: false,
            }),
            released: Condvar::new(),
        }
    }

    fn acquire(&self, exclusive: bool) -> Result<(), Errno> {
        let mut holders = self.holders.lock();

        while holders.writer || (exclusive && holders.readers != 0) {
            holders = self.released.wait(holders);
        }

        if exclusive {
            holders.writer = true;
        } else {
            holders.readers = holders.readers.checked_add(1).ok_or(Errno::Again)?;
        }
        Ok(())
    }

    fn release(&self) -> Result<(), Errno> {
        let mut holders = self.holders.lock();

        if holders.writer {
            holders.writer = false;
        } else {
            holders.readers = holders.readers.checked_sub(1).ok_or(Errno::NotPermitted)?;
        }

        if holders.readers == 0 {
            self.released.notify_all();
        }
        Ok(())
    }
}

#[unsafe(no_mangle)]
extern "C" fn pthread_rwlock_init(
    rwlock: Option<&mut MaybeUninit<RwLock>>,
    _attr: *const c_void,
) -> c_int {
    let Some(rwlock) = rwlock else {
        return Errno::InvalidArgument.code();
    };

    rwlock.write(RwLock::new());
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_rwlock_destroy(rwlock: Option<&RwLock>) -> c_int {
    // Ожидающий поток выходит из захвата, только получив блокировку. Уничтожение, которое
    // допускает POSIX, поэтому застаёт условную переменную без ожидающих потоков.
    match rwlock {
        Some(_) => 0,
        None => Errno::InvalidArgument.code(),
    }
}

#[unsafe(no_mangle)]
extern "C" fn pthread_rwlock_rdlock(rwlock: Option<&RwLock>) -> c_int {
    let Some(rwlock) = rwlock else {
        return Errno::InvalidArgument.code();
    };

    match rwlock.acquire(false) {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
}

#[unsafe(no_mangle)]
extern "C" fn pthread_rwlock_wrlock(rwlock: Option<&RwLock>) -> c_int {
    let Some(rwlock) = rwlock else {
        return Errno::InvalidArgument.code();
    };

    match rwlock.acquire(true) {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
}

#[unsafe(no_mangle)]
extern "C" fn pthread_rwlock_unlock(rwlock: Option<&RwLock>) -> c_int {
    let Some(rwlock) = rwlock else {
        return Errno::InvalidArgument.code();
    };

    match rwlock.release() {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
}
