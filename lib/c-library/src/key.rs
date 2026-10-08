//! Ключи локальных данных потока библиотеки C поверх ключей `runtime::tls`.
//!
//! `pthread_key_t` - ключ `runtime::tls`, деструктор значения передаётся слою как есть.
//! Снятый ключ слой не выдаёт повторно.

#![allow(unsafe_code)]

use core::ffi::{c_int, c_uint, c_void};

use runtime::tls::{self, Dtor};

use crate::errno::Errno;

#[unsafe(no_mangle)]
extern "C" fn pthread_key_create(key: Option<&mut c_uint>, destructor: Option<Dtor>) -> c_int {
    let Some(key) = key else {
        return Errno::InvalidArgument.code();
    };

    let Ok(created) = c_uint::try_from(tls::create(destructor)) else {
        return Errno::Again.code();
    };
    if created == 0 {
        return Errno::Again.code();
    }

    *key = created;
    0
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pthread_key_delete(key: c_uint) -> c_int {
    // SAFETY: `key` выдан `pthread_key_create` по контракту C.
    unsafe { tls::destroy(key as usize) };
    0
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pthread_getspecific(key: c_uint) -> *mut c_void {
    // SAFETY: `key` выдан `pthread_key_create` по контракту C.
    unsafe { tls::get(key as usize) }.cast()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pthread_setspecific(key: c_uint, value: *const c_void) -> c_int {
    let value = value.cast_mut().cast::<u8>();
    // SAFETY: `key` выдан `pthread_key_create` по контракту C.
    unsafe { tls::set(key as usize, value) };
    // SAFETY: см. выше.
    if unsafe { tls::get(key as usize) } != value {
        return Errno::OutOfMemory.code();
    }
    0
}
