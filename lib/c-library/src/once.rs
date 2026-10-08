//! Однократная инициализация POSIX `pthread_once` над словом `pthread_once_t`.

#![allow(unsafe_code)]

use core::{
    ffi::c_int,
    sync::atomic::{
        AtomicU32,
        Ordering::{Acquire, Relaxed, Release},
    },
};

use runtime::{futex_wait, futex_wake_all};

use crate::errno::Errno;

const INCOMPLETE: u32 = 0;
const RUNNING: u32 = 1;
const WAITED: u32 = 2;
const COMPLETE: u32 = 3;

type Init = unsafe extern "C" fn();

#[unsafe(no_mangle)]
extern "C" fn pthread_once(once: Option<&AtomicU32>, init: Option<Init>) -> c_int {
    let (Some(once), Some(init)) = (once, init) else {
        return Errno::InvalidArgument.code();
    };

    loop {
        match once.compare_exchange(INCOMPLETE, RUNNING, Acquire, Acquire) {
            Ok(_) => {
                // SAFETY: функцию передала программа для вызова без аргументов.
                unsafe { init() };

                if once.swap(COMPLETE, Release) == WAITED {
                    futex_wake_all(once);
                }

                return 0;
            }
            Err(COMPLETE) => return 0,
            Err(RUNNING) => {
                let _ = once.compare_exchange(RUNNING, WAITED, Relaxed, Relaxed);
            }
            Err(_) => {}
        }

        futex_wait(once, WAITED, None);
    }
}
