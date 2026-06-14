//! E2E проверка `EventCreate`/`ObjectSignal`/`ObjectWaitOne` из EL0.

use kernel_tests::kernel_test;
use runtime::{event_create, object_signal, object_wait_one};
use syscall::EVENT_SIGNALED;

/// Создаёт Event, поднимает `EVENT_SIGNALED` через `object_signal`
/// (count=0 - разбудить всех) и проверяет, что poll (timeout 0) видит бит.
#[kernel_test]
fn event_signal_wait_round_trip() {
    let event = event_create().expect("event_create must succeed");

    kernel_tests::kassert_eq!(object_signal(event, EVENT_SIGNALED, 0, 0), 0);

    let observed = object_wait_one(event, EVENT_SIGNALED, 0);
    kernel_tests::kassert!(observed >= 0);
    let bit = i64::from(EVENT_SIGNALED);
    kernel_tests::kassert!(observed & bit == bit);
}
