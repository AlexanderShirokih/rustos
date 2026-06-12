//! Сигнал `Event` будит зарегистрированный `Waker`.

extern crate alloc;

use alloc::sync::Arc;

use kernel_tests::kernel_test;

#[kernel_test]
fn event_signal() {
    use core::sync::atomic::{AtomicBool, Ordering};

    use kobject::{EVENT_SIGNALED, Event, Waker};

    struct Flag {
        fired: AtomicBool,
    }
    impl Waker for Flag {
        fn wake(&self, _observed: u32) {
            self.fired.store(true, Ordering::Release);
        }
    }

    let event = Event::new();
    kernel_tests::kassert_eq!(event.peek(), 0);

    let flag = Arc::new(Flag {
        fired: AtomicBool::new(false),
    });
    event
        .signals()
        .register_waiter(EVENT_SIGNALED, flag.clone());
    kernel_tests::kassert!(!flag.fired.load(Ordering::Acquire));

    event.signal(EVENT_SIGNALED, 0);
    kernel_tests::kassert!(flag.fired.load(Ordering::Acquire));
    kernel_tests::kassert!(event.peek() & EVENT_SIGNALED != 0);
}
