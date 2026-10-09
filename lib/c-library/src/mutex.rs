//! Мьютекс POSIX `pthread_mutex_t` видов `PTHREAD_MUTEX_NORMAL`, `RECURSIVE` и `ERRORCHECK`
//! поверх замка с владельцем [`Lock`] и его атрибуты `pthread_mutexattr_t`.

#![allow(unsafe_code)]

use core::{ffi::c_int, mem::MaybeUninit};

use crate::{
    clock::{self, Clock},
    errno::Errno,
    lock::Lock,
};

// Значения определены в `pthread.c` константами и не меняются.
unsafe extern "C" {
    safe static c_library_pthread_mutex_normal: c_int;
    safe static c_library_pthread_mutex_recursive: c_int;
    safe static c_library_pthread_mutex_errorcheck: c_int;
    safe static c_library_pthread_mutex_default: c_int;
}

const _: () = assert!(size_of::<Mutex>() == 32 && align_of::<Mutex>() == 8);

#[repr(C)]
pub(crate) struct Mutex {
    lock: Lock,
    kind: c_int,
}

#[repr(C)]
struct Attributes {
    kind: c_int,
}

impl Mutex {
    pub(crate) fn lock(&self) -> &Lock {
        &self.lock
    }

    pub(crate) fn checks_owner(&self) -> bool {
        self.recursive() || self.kind == c_library_pthread_mutex_errorcheck
    }

    fn recursive(&self) -> bool {
        self.kind == c_library_pthread_mutex_recursive
    }

    fn relock(&self) -> Option<c_int> {
        if !self.checks_owner() || !self.lock.owned() {
            return None;
        }

        if !self.recursive() {
            return Some(Errno::Deadlock.code());
        }

        Some(reenter(self))
    }
}

fn known(kind: c_int) -> bool {
    kind == c_library_pthread_mutex_normal
        || kind == c_library_pthread_mutex_recursive
        || kind == c_library_pthread_mutex_errorcheck
        || kind == c_library_pthread_mutex_default
}

#[unsafe(no_mangle)]
extern "C" fn pthread_mutex_init(
    mutex: Option<&mut MaybeUninit<Mutex>>,
    attr: Option<&Attributes>,
) -> c_int {
    let Some(mutex) = mutex else {
        return Errno::InvalidArgument.code();
    };

    let kind = attr.map_or(c_library_pthread_mutex_default, |attr| attr.kind);
    mutex.write(Mutex {
        lock: Lock::new(),
        kind,
    });
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_mutex_destroy(_mutex: *mut Mutex) -> c_int {
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_mutex_lock(mutex: Option<&Mutex>) -> c_int {
    let Some(mutex) = mutex else {
        return Errno::InvalidArgument.code();
    };

    if let Some(status) = mutex.relock() {
        return status;
    }

    mutex.lock.acquire();
    0
}

#[unsafe(no_mangle)]
extern "C" fn c_library_mutex_timedlock(
    mutex: Option<&Mutex>,
    seconds: i64,
    nanoseconds: i64,
) -> c_int {
    let Some(mutex) = mutex else {
        return Errno::InvalidArgument.code();
    };

    let timeout = match clock::remaining(Clock::Realtime, seconds, nanoseconds) {
        Ok(timeout) => timeout,
        Err(error) => return error.code(),
    };

    if let Some(status) = mutex.relock() {
        return status;
    }

    if mutex.lock.acquire_timeout(timeout) {
        0
    } else {
        Errno::TimedOut.code()
    }
}

#[unsafe(no_mangle)]
extern "C" fn pthread_mutex_trylock(mutex: Option<&Mutex>) -> c_int {
    let Some(mutex) = mutex else {
        return Errno::InvalidArgument.code();
    };

    if mutex.recursive() && mutex.lock.owned() {
        return reenter(mutex);
    }

    if mutex.lock.try_acquire() {
        0
    } else {
        Errno::Busy.code()
    }
}

#[unsafe(no_mangle)]
extern "C" fn pthread_mutex_unlock(mutex: Option<&Mutex>) -> c_int {
    let Some(mutex) = mutex else {
        return Errno::InvalidArgument.code();
    };

    if mutex.checks_owner() && !mutex.lock.owned() {
        return Errno::NotPermitted.code();
    }

    // SAFETY: вид с учётом владельца проверен выше; нормальный мьютекс освобождает его
    // владелец по контракту C.
    unsafe { mutex.lock.release() };
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_mutexattr_init(attr: Option<&mut MaybeUninit<Attributes>>) -> c_int {
    let Some(attr) = attr else {
        return Errno::InvalidArgument.code();
    };

    attr.write(Attributes {
        kind: c_library_pthread_mutex_default,
    });
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_mutexattr_destroy(_attr: *mut Attributes) -> c_int {
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_mutexattr_settype(attr: Option<&mut Attributes>, kind: c_int) -> c_int {
    let Some(attr) = attr else {
        return Errno::InvalidArgument.code();
    };

    if !known(kind) {
        return Errno::InvalidArgument.code();
    }

    attr.kind = kind;
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_mutexattr_gettype(
    attr: Option<&Attributes>,
    kind: Option<&mut c_int>,
) -> c_int {
    let (Some(attr), Some(kind)) = (attr, kind) else {
        return Errno::InvalidArgument.code();
    };

    *kind = attr.kind;
    0
}

fn reenter(mutex: &Mutex) -> c_int {
    if mutex.lock.reenter() {
        0
    } else {
        Errno::Again.code()
    }
}
