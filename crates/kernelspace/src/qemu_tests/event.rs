//! Сигнал `Event` будит зарегистрированный `Waker`.

extern crate alloc;

use alloc::sync::Arc;

use test_harness_qemu::register_test;

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
    test_harness_qemu::kassert_eq!(event.peek(), 0);

    let flag = Arc::new(Flag {
        fired: AtomicBool::new(false),
    });
    event
        .signals()
        .register_waiter(EVENT_SIGNALED, flag.clone());
    test_harness_qemu::kassert!(!flag.fired.load(Ordering::Acquire));

    event.signal(EVENT_SIGNALED, 0);
    test_harness_qemu::kassert!(flag.fired.load(Ordering::Acquire));
    test_harness_qemu::kassert!(event.peek() & EVENT_SIGNALED != 0);
}

register_test!(EVENT_SIGNAL, "event_signal", event_signal);
