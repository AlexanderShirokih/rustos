//! E2E проверка `SignalCreate`/`SignalSet`/`SignalWaitOne` из EL0.

use kernel_tests::kernel_test;
use runtime::{Signal, Timeout};
use syscall::{SIGNALED, WakeCount};

#[kernel_test]
fn signal_wait_round_trip() {
    let signal = Signal::create().expect("signal create must succeed");

    signal
        .set(SIGNALED, 0, WakeCount::All)
        .expect("signal set must succeed");

    let observed = signal
        .wait(SIGNALED, Timeout::POLL)
        .expect("signal wait must succeed");
    kernel_tests::kassert!(observed & SIGNALED == SIGNALED);
}
