//! Signal capability target + `signal_wait_one` через scheduler-блокировку:
//! проверяем, что сигнал, поднятый отдельным потоком после `sleep_ms`,
//! корректно будит ожидающего на signal-бите.

use capability::{Capability, CapabilityTarget, Rights, Signal, install_handle, signal_wait_one};
use kernel_tests::kernel_test;
use scheduler::{Priority, SchedulerServiceExt, SpawnConfig};

const SIGNAL_BIT: u32 = 1 << 0;

#[kernel_test]
fn signal_after_deadline() {
    let signal = Signal::new();

    let signal_handle = Capability::new(CapabilityTarget::Signal(signal.clone()), Rights::READ);
    let signal_id = install_handle(signal_handle).expect("install_handle must succeed");

    let signal_for_raise = signal.clone();
    let scheduler = super::scheduler().clone();
    let scheduler_for_signal = scheduler.clone();
    scheduler
        .spawn(
            SpawnConfig::new("signal-test-signaler").priority(Priority::highest()),
            move || {
                scheduler_for_signal.sleep_ms(50);
                signal_for_raise.signal(SIGNAL_BIT, 0);
            },
        )
        .expect("signaler spawn must succeed");

    let observed =
        signal_wait_one(signal_id, SIGNAL_BIT, None).expect("wait must complete via SIGNAL_BIT");

    kernel_tests::kassert!(observed & SIGNAL_BIT == SIGNAL_BIT);
    kernel_tests::kassert!(signal.peek() & SIGNAL_BIT == SIGNAL_BIT);
}
