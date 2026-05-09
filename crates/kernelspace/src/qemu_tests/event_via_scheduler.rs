//! Event KO + `object_wait_one` через scheduler-блокировку: проверяем,
//! что сигнал, поднятый отдельным потоком после `sleep_ms`, корректно
//! будит ожидающего на signal-бите.

use kobject::{Event, Handle, KObject, Rights, install_handle, object_wait_one};
use scheduler::{Priority, SchedulerServiceExt, SpawnConfig};
use test_harness_qemu::register_test;

const SIGNAL_BIT: u32 = 1 << 0;

fn event_signal_after_deadline() {
    let event = Event::new();

    let event_handle = Handle::new(
        KObject::Event(event.clone()),
        Rights::WAIT | Rights::INSPECT,
    );
    let event_id = install_handle(event_handle).expect("install_handle must succeed");

    let event_for_signal = event.clone();
    let scheduler = super::scheduler().clone();
    let scheduler_for_signal = scheduler.clone();
    scheduler
        .spawn(
            SpawnConfig::new("event-test-signaler").priority(Priority::highest()),
            move || {
                scheduler_for_signal.sleep_ms(50);
                event_for_signal.signal(SIGNAL_BIT, 0);
            },
        )
        .expect("signaler spawn must succeed");

    let observed =
        object_wait_one(event_id, SIGNAL_BIT, None).expect("wait must complete via SIGNAL_BIT");

    test_harness_qemu::kassert!(observed & SIGNAL_BIT == SIGNAL_BIT);
    test_harness_qemu::kassert!(event.peek() & SIGNAL_BIT == SIGNAL_BIT);
}

register_test!(
    EVENT_SIGNAL_AFTER_DEADLINE,
    "event_signal_after_deadline",
    event_signal_after_deadline
);
