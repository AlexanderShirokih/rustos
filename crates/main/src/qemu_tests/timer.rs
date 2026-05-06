//! `Timer` KO + `object_wait_one` через scheduler-блокировку: проверяем,
//! что сигнал, поднятый отдельным потоком после `sleep_ms`, корректно
//! будит ожидающего на `TIMER_SIGNALED`.

extern crate alloc;

use alloc::sync::Arc;

use drivers_common::services::scheduler::{Priority, SchedulerServiceExt, SpawnConfig};
use qemu_test_harness::register_test;

use crate::kobject::{
    Handle, KernelObject, Rights, TIMER_SIGNALED, Timer, install_handle, object_wait_one,
};

fn timer_signal_after_deadline() {
    let timer = Timer::new();

    let timer_handle = Handle::new(
        timer.clone() as Arc<dyn KernelObject>,
        Rights::WAIT | Rights::INSPECT,
    );
    let timer_id = install_handle(timer_handle).expect("install_handle must succeed");

    let timer_for_signal = timer.clone();
    let scheduler = super::scheduler().clone();
    let scheduler_for_signal = scheduler.clone();
    scheduler
        .spawn(
            SpawnConfig::new("timer-test-signaler").priority(Priority::highest()),
            move || {
                scheduler_for_signal.sleep_ms(50);
                timer_for_signal.fire();
            },
        )
        .expect("signaler spawn must succeed");

    let observed =
        object_wait_one(timer_id, TIMER_SIGNALED, None).expect("wait must complete via SIGNALED");

    qemu_test_harness::kassert!(observed & TIMER_SIGNALED == TIMER_SIGNALED);
    qemu_test_harness::kassert!(timer.peek() & TIMER_SIGNALED == TIMER_SIGNALED);
}

register_test!(
    TIMER_SIGNAL_AFTER_DEADLINE,
    "timer_signal_after_deadline",
    timer_signal_after_deadline
);
