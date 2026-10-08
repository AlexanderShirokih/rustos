//! E2E монотонных часов из EL0.

use kernel_objects::{Error, Signal};
use runtime::{Instant, Timeout};
use syscall::{SIGNALED, SyscallError};

const PAUSE: Timeout = Timeout::from_millis(5);
const MONOTONIC_SAMPLES: usize = 256;

#[test_harness::test]
fn clock_monotonic_never_goes_back() {
    let mut previous = Instant::now();
    for _ in 0..MONOTONIC_SAMPLES {
        let now = Instant::now();
        test_harness::check!(now >= previous);
        previous = now;
    }
}

#[test_harness::test]
fn clock_monotonic_advances_over_a_pause() {
    let idle = Signal::create().expect("create idle signal");

    let before = Instant::now();
    let observed = idle.wait(SIGNALED, PAUSE);
    let elapsed = Instant::now().since(before);

    test_harness::check_eq!(observed, Err(Error::Syscall(SyscallError::Timeout)));
    test_harness::check!(elapsed.raw() >= PAUSE.raw());
}
