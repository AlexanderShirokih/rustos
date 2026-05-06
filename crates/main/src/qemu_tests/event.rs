//! Сигнал `Event` будит зарегистрированный `Waker`.

extern crate alloc;

use alloc::sync::Arc;

use qemu_test_harness::register_test;

fn event_signal() {
    use core::sync::atomic::{AtomicBool, Ordering};

    use crate::kobject::{EVENT_SIGNALED, Event, Waker};

    struct Flag {
        fired: AtomicBool,
    }
    impl Waker for Flag {
        fn wake(&self, _observed: u32) {
            self.fired.store(true, Ordering::Release);
        }
    }

    let event = Event::new();
    qemu_test_harness::kassert_eq!(event.peek(), 0);

    let flag = Arc::new(Flag {
        fired: AtomicBool::new(false),
    });
    event
        .signals()
        .register_waiter(EVENT_SIGNALED, flag.clone());
    qemu_test_harness::kassert!(!flag.fired.load(Ordering::Acquire));

    event.signal(EVENT_SIGNALED, 0);
    qemu_test_harness::kassert!(flag.fired.load(Ordering::Acquire));
    qemu_test_harness::kassert!(event.peek() & EVENT_SIGNALED != 0);
}

register_test!(EVENT_SIGNAL, "event_signal", event_signal);
