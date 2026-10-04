//! E2E проверка `SignalCreate`/`SignalSet`/`SignalWaitOne`/`SignalWaitMany`
//! из EL0.

use core::num::NonZeroU32;

use kernel_objects::{Signal, kind, wait_any};
use kernel_tests::kernel_test;
use runtime::{Priority, Timeout, signal_set, spawn};
use syscall::{Handle, SIGNALED, SyscallError, WAIT_MANY_MAX_COUNT, WaitItem, WakeCount};

const STACK_SIZE: u64 = 0x4000;
const WAKE_TIMEOUT: Timeout = Timeout::from_secs(5);
const WORKER_DELAY: Timeout = Timeout::from_millis(50);

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

fn sleep(budget: Timeout) {
    let idle = Signal::create().expect("signal create for sleep");
    kernel_tests::kassert!(idle.wait(SIGNALED, budget).is_err());
}

extern "C" fn delayed_signaler(raw: usize) -> u32 {
    sleep(WORKER_DELAY);
    let handle = NonZeroU32::new(raw as u32)
        .map(Handle::<kind::Signal>::from_abi)
        .expect("signal handle id");
    u32::from(signal_set(handle, SIGNALED, 0, WakeCount::All) != 0)
}

#[kernel_test]
fn wait_blocks_until_another_thread_signals() {
    let signal = Signal::create().expect("signal create must succeed");
    let raw = signal.as_raw().raw() as usize;

    let worker =
        spawn(delayed_signaler, raw, STACK_SIZE, Priority::new(1)).expect("spawn signaler");

    kernel_tests::kassert!(signal.wait(SIGNALED, Timeout::POLL).is_err());

    let observed = signal
        .wait(SIGNALED, WAKE_TIMEOUT)
        .expect("waiter must be woken by the signaler");
    kernel_tests::kassert!(observed & SIGNALED == SIGNALED);

    worker
        .join(WAKE_TIMEOUT)
        .expect("signaler joins before timeout");
}

#[kernel_test]
fn wait_many_returns_the_index_of_the_raised_signal() {
    let idle = Signal::create().expect("idle signal create");
    let raised = Signal::create().expect("raised signal create");
    raised
        .set(SIGNALED, 0, WakeCount::All)
        .expect("signal set must succeed");

    let items = [
        WaitItem::new(idle.as_raw(), SIGNALED),
        WaitItem::new(raised.as_raw(), SIGNALED),
    ];
    let index = wait_any(&items, Timeout::POLL).expect("raised entry satisfies the wait");
    kernel_tests::kassert_eq!(index, 1);
}

#[kernel_test]
fn wait_many_blocks_until_another_thread_signals() {
    let idle = Signal::create().expect("idle signal create");
    let target = Signal::create().expect("target signal create");
    let raw = target.as_raw().raw() as usize;

    let worker =
        spawn(delayed_signaler, raw, STACK_SIZE, Priority::new(1)).expect("spawn signaler");

    let items = [
        WaitItem::new(idle.as_raw(), SIGNALED),
        WaitItem::new(target.as_raw(), SIGNALED),
    ];
    kernel_tests::kassert!(matches!(
        wait_any(&items, Timeout::POLL),
        Err(kernel_objects::Error::Syscall(SyscallError::Timeout))
    ));

    let index = wait_any(&items, WAKE_TIMEOUT).expect("waiter must be woken by the signaler");
    kernel_tests::kassert_eq!(index, 1);

    worker
        .join(WAKE_TIMEOUT)
        .expect("signaler joins before timeout");
}

#[kernel_test]
fn wait_many_rejects_a_set_outside_the_bound() {
    let signal = Signal::create().expect("signal create");
    let item = WaitItem::new(signal.as_raw(), SIGNALED);

    kernel_tests::kassert!(matches!(
        wait_any(&[], Timeout::POLL),
        Err(kernel_objects::Error::Syscall(
            SyscallError::InvalidArgument
        ))
    ));

    let oversized = [item; WAIT_MANY_MAX_COUNT + 1];
    kernel_tests::kassert!(matches!(
        wait_any(&oversized, Timeout::POLL),
        Err(kernel_objects::Error::Syscall(
            SyscallError::InvalidArgument
        ))
    ));
}
