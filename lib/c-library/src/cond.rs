//! Условная переменная POSIX `pthread_cond_t` поверх `runtime::RawCondvar` и её атрибуты
//! `pthread_condattr_t`.

#![allow(unsafe_code)]

use core::{ffi::c_int, mem::MaybeUninit};

use runtime::{RawCondvar, RawMutex};

use crate::{
    clock::{self, Clock, ClockId},
    errno::Errno,
    mutex::Mutex,
};

const REALTIME_FIELD: c_int = 0;
const MONOTONIC_FIELD: c_int = 1;

const _: () = assert!(size_of::<Cond>() == 12 && align_of::<Cond>() == 4);

#[repr(C)]
struct Cond {
    raw: RawCondvar,
    clock: c_int,
}

#[repr(C)]
struct Attributes {
    clock: c_int,
}

fn field(clock: Clock) -> c_int {
    match clock {
        Clock::Realtime => REALTIME_FIELD,
        Clock::Monotonic => MONOTONIC_FIELD,
    }
}

fn clock(field: c_int) -> Clock {
    // Нулевое поле переменной `PTHREAD_COND_INITIALIZER` означает `CLOCK_REALTIME`.
    if field == MONOTONIC_FIELD {
        Clock::Monotonic
    } else {
        Clock::Realtime
    }
}

fn with_mutex_released<T>(mutex: &Mutex, wait: impl FnOnce(&RawMutex) -> T) -> Result<T, Errno> {
    let lock = mutex.lock();
    if mutex.checks_owner() && !lock.owned() {
        return Err(Errno::NotPermitted);
    }

    let depth = lock.suspend();
    let outcome = wait(lock.mutex());
    lock.resume(depth);
    Ok(outcome)
}

#[unsafe(no_mangle)]
extern "C" fn pthread_cond_init(
    cond: Option<&mut MaybeUninit<Cond>>,
    attr: Option<&Attributes>,
) -> c_int {
    let Some(cond) = cond else {
        return Errno::InvalidArgument.code();
    };

    cond.write(Cond {
        raw: RawCondvar::new(),
        clock: attr.map_or(REALTIME_FIELD, |attr| attr.clock),
    });
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_cond_destroy(cond: Option<&Cond>) -> c_int {
    let Some(cond) = cond else {
        return Errno::InvalidArgument.code();
    };

    cond.raw.destroy();
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_cond_wait(cond: Option<&Cond>, mutex: Option<&Mutex>) -> c_int {
    let (Some(cond), Some(mutex)) = (cond, mutex) else {
        return Errno::InvalidArgument.code();
    };

    // SAFETY: `with_mutex_released` передаёт мьютекс, который держит вызывающий. Мьютекс с
    // учётом владельца она проверяет, нормальный мьютекс вызывающий держит по контракту C.
    match with_mutex_released(mutex, |raw| unsafe { cond.raw.wait(raw) }) {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
}

#[unsafe(no_mangle)]
extern "C" fn c_library_cond_timedwait(
    cond: Option<&Cond>,
    mutex: Option<&Mutex>,
    deadline_clock_id: Option<&ClockId>,
    seconds: i64,
    nanoseconds: i64,
) -> c_int {
    let (Some(cond), Some(mutex)) = (cond, mutex) else {
        return Errno::InvalidArgument.code();
    };
    let deadline_clock = match deadline_clock_id {
        Some(&id) => match Clock::from_id(id) {
            Some(clock) => clock,
            None => return Errno::InvalidArgument.code(),
        },
        None => clock(cond.clock),
    };

    let timeout = match clock::remaining(deadline_clock, seconds, nanoseconds) {
        Ok(timeout) => timeout,
        Err(error) => return error.code(),
    };

    // SAFETY: `with_mutex_released` передаёт мьютекс, который держит вызывающий. Мьютекс с
    // учётом владельца она проверяет, нормальный мьютекс вызывающий держит по контракту C.
    match with_mutex_released(mutex, |raw| unsafe { cond.raw.wait_timeout(raw, timeout) }) {
        Ok(true) => 0,
        Ok(false) => Errno::TimedOut.code(),
        Err(error) => error.code(),
    }
}

#[unsafe(no_mangle)]
extern "C" fn pthread_cond_signal(cond: Option<&Cond>) -> c_int {
    let Some(cond) = cond else {
        return Errno::InvalidArgument.code();
    };

    cond.raw.notify_one();
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_cond_broadcast(cond: Option<&Cond>) -> c_int {
    let Some(cond) = cond else {
        return Errno::InvalidArgument.code();
    };

    cond.raw.notify_all();
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_condattr_init(attr: Option<&mut MaybeUninit<Attributes>>) -> c_int {
    let Some(attr) = attr else {
        return Errno::InvalidArgument.code();
    };

    attr.write(Attributes {
        clock: REALTIME_FIELD,
    });
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_condattr_destroy(_attr: *mut Attributes) -> c_int {
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_condattr_setclock(attr: Option<&mut Attributes>, id: ClockId) -> c_int {
    let (Some(attr), Some(clock)) = (attr, Clock::from_id(id)) else {
        return Errno::InvalidArgument.code();
    };

    attr.clock = field(clock);
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_condattr_getclock(
    attr: Option<&Attributes>,
    id: Option<&mut ClockId>,
) -> c_int {
    let (Some(attr), Some(id)) = (attr, id) else {
        return Errno::InvalidArgument.code();
    };

    *id = clock(attr.clock).id();
    0
}
