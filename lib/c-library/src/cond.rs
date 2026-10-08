//! Условная переменная POSIX `pthread_cond_t` поверх `runtime::RawCondvar` и её атрибуты
//! `pthread_condattr_t`.

#![allow(unsafe_code)]

use core::{ffi::c_int, mem::MaybeUninit};

use runtime::RawCondvar;

use crate::{errno::Errno, mutex::Mutex};

const _: () = assert!(size_of::<RawCondvar>() == 8 && align_of::<RawCondvar>() == 4);

#[repr(C)]
struct Attributes {
    reserved: c_int,
}

#[unsafe(no_mangle)]
extern "C" fn pthread_cond_init(
    cond: Option<&mut MaybeUninit<RawCondvar>>,
    _attr: *const Attributes,
) -> c_int {
    let Some(cond) = cond else {
        return Errno::InvalidArgument.code();
    };

    cond.write(RawCondvar::new());
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_cond_destroy(cond: Option<&RawCondvar>) -> c_int {
    let Some(cond) = cond else {
        return Errno::InvalidArgument.code();
    };

    cond.destroy();
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_cond_wait(cond: Option<&RawCondvar>, mutex: Option<&Mutex>) -> c_int {
    let (Some(cond), Some(mutex)) = (cond, mutex) else {
        return Errno::InvalidArgument.code();
    };

    let lock = mutex.lock();
    if mutex.checks_owner() && !lock.owned() {
        return Errno::NotPermitted.code();
    }

    let depth = lock.suspend();
    // SAFETY: вызывающий держит мьютекс: вид с учётом владельца проверен выше, нормальный
    // мьютекс держит по контракту C.
    unsafe { cond.wait(lock.mutex()) };
    lock.resume(depth);
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_cond_signal(cond: Option<&RawCondvar>) -> c_int {
    let Some(cond) = cond else {
        return Errno::InvalidArgument.code();
    };

    cond.notify_one();
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_cond_broadcast(cond: Option<&RawCondvar>) -> c_int {
    let Some(cond) = cond else {
        return Errno::InvalidArgument.code();
    };

    cond.notify_all();
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_condattr_init(attr: Option<&mut MaybeUninit<Attributes>>) -> c_int {
    let Some(attr) = attr else {
        return Errno::InvalidArgument.code();
    };

    attr.write(Attributes { reserved: 0 });
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_condattr_destroy(_attr: *mut Attributes) -> c_int {
    0
}
