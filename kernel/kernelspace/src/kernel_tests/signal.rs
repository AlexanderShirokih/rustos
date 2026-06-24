//! Сигнал `Signal` будит зарегистрированный `Waker`.

extern crate alloc;

use alloc::sync::Arc;

use kernel_tests::kernel_test;

#[kernel_test]
fn signal_wakes_waker() {
    use core::sync::atomic::{AtomicBool, Ordering};

    use capability::{SIGNALED, Signal, Waker};

    struct Flag {
        fired: AtomicBool,
    }
    impl Waker for Flag {
        fn wake(&self, _observed: u32) {
            self.fired.store(true, Ordering::Release);
        }
    }

    let signal = Signal::new();
    kernel_tests::kassert_eq!(signal.peek(), 0);

    let flag = Arc::new(Flag {
        fired: AtomicBool::new(false),
    });
    signal.register_waiter(SIGNALED, flag.clone());
    kernel_tests::kassert!(!flag.fired.load(Ordering::Acquire));

    signal.signal(SIGNALED, 0);
    kernel_tests::kassert!(flag.fired.load(Ordering::Acquire));
    kernel_tests::kassert!(signal.peek() & SIGNALED != 0);
}
