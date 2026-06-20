//! E2E проверка `SignalCreate`/`SignalSet`/`SignalWaitOne` из EL0.

use kernel_tests::kernel_test;
use runtime::{signal_create, signal_set, signal_wait_one};
use syscall::SIGNALED;

/// Создаёт `Signal`, поднимает `SIGNALED` через `signal_set`
/// (count=0 - разбудить всех) и проверяет, что poll (timeout 0) видит бит.
#[kernel_test]
fn signal_wait_round_trip() {
    let signal = signal_create().expect("signal_create must succeed");

    kernel_tests::kassert_eq!(signal_set(signal, SIGNALED, 0, 0), 0);

    let observed = signal_wait_one(signal, SIGNALED, 0);
    kernel_tests::kassert!(observed >= 0);
    let bit = i64::from(SIGNALED);
    kernel_tests::kassert!(observed & bit == bit);
}
