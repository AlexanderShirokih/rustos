//! Round-trip handle-based Event API: `event_create` -> `object_signal`
//! -> `object_wait_one` видит поднятый бит; плюс `count` ограничивает
//! число разбуженных waiter'ов в FIFO-порядке.

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use kernel_tests::kernel_test;
use kobject::{
    EVENT_SIGNALED, Event, Handle, KObject, Rights, Waker, event_create, install_handle,
    object_signal, object_wait_one,
};

#[kernel_test]
fn event_create_signal_wait_round_trip() {
    let event = event_create().expect("event_create must succeed");

    object_signal(event, EVENT_SIGNALED, 0, 0).expect("signal must succeed");

    let observed =
        object_wait_one(event, EVENT_SIGNALED, Some(0)).expect("poll must see EVENT_SIGNALED");
    kernel_tests::kassert!(observed & EVENT_SIGNALED == EVENT_SIGNALED);
}

#[kernel_test]
fn object_signal_count_wakes_at_most_n() {
    struct Flag {
        fired: AtomicBool,
    }
    impl Waker for Flag {
        fn wake(&self, _observed: u32) {
            self.fired.store(true, Ordering::Release);
        }
    }

    let event = Event::new();
    let id = install_handle(Handle::new(
        KObject::Event(event.clone()),
        Rights::SIGNAL | Rights::WAIT,
    ))
    .expect("install_handle must succeed");

    let w1 = Arc::new(Flag {
        fired: AtomicBool::new(false),
    });
    let w2 = Arc::new(Flag {
        fired: AtomicBool::new(false),
    });
    event.signals().register_waiter(EVENT_SIGNALED, w1.clone());
    event.signals().register_waiter(EVENT_SIGNALED, w2.clone());

    object_signal(id, EVENT_SIGNALED, 0, 1).expect("signal must succeed");

    kernel_tests::kassert!(w1.fired.load(Ordering::Acquire));
    kernel_tests::kassert!(!w2.fired.load(Ordering::Acquire));
}
