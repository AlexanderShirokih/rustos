//! Round-trip handle-based Signal API: `signal_create` ->
//! `signal_set` -> `signal_wait_one` видит поднятый бит; плюс `count`
//! ограничивает число разбуженных waiter'ов в FIFO-порядке.

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use kernel_tests::kernel_test;
use kobject::{
    Handle, KObject, Rights, SIGNALED, Signal, Waker, install_handle, signal_create, signal_set,
    signal_wait_one,
};

#[kernel_test]
fn signal_create_signal_wait_round_trip() {
    let signal = signal_create().expect("signal_create must succeed");

    signal_set(signal, SIGNALED, 0, 0).expect("signal must succeed");

    let observed = signal_wait_one(signal, SIGNALED, Some(0)).expect("poll must see SIGNALED");
    kernel_tests::kassert!(observed & SIGNALED == SIGNALED);
}

#[kernel_test]
fn signal_set_count_wakes_at_most_n() {
    struct Flag {
        fired: AtomicBool,
    }
    impl Waker for Flag {
        fn wake(&self, _observed: u32) {
            self.fired.store(true, Ordering::Release);
        }
    }

    let signal = Signal::new();
    let id = install_handle(Handle::new(
        KObject::Signal(signal.clone()),
        Rights::WRITE | Rights::READ,
    ))
    .expect("install_handle must succeed");

    let w1 = Arc::new(Flag {
        fired: AtomicBool::new(false),
    });
    let w2 = Arc::new(Flag {
        fired: AtomicBool::new(false),
    });
    signal.register_waiter(SIGNALED, w1.clone());
    signal.register_waiter(SIGNALED, w2.clone());

    signal_set(id, SIGNALED, 0, 1).expect("signal must succeed");

    kernel_tests::kassert!(w1.fired.load(Ordering::Acquire));
    kernel_tests::kassert!(!w2.fired.load(Ordering::Acquire));
}
