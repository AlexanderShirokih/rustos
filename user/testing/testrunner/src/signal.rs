//! E2E проверка `SignalCreate`/`SignalSet`/`SignalWaitOne`/`SignalWaitMany`
//! из EL0.

use core::num::NonZeroU32;

use kernel_objects::{IssuedHandle, OwnedHandle, Signal, kind, wait_any};
use runtime::{Timeout, signal_set, spawn};
use syscall::{Handle, Rights, SIGNALED, SyscallError, WAIT_MANY_MAX_COUNT, WaitItem, WakeCount};

const WAKE_TIMEOUT: Timeout = Timeout::from_secs(5);
const WORKER_DELAY: Timeout = Timeout::from_millis(50);

const REVOKE_RACE_ATTEMPTS: usize = 3;

#[test_harness::test]
fn signal_wait_round_trip() {
    let signal = Signal::create().expect("signal create must succeed");

    signal
        .set(SIGNALED, 0, WakeCount::All)
        .expect("signal set must succeed");

    let observed = signal
        .wait(SIGNALED, Timeout::POLL)
        .expect("signal wait must succeed");
    test_harness::check!(observed & SIGNALED == SIGNALED);
}

fn sleep(budget: Timeout) {
    let idle = Signal::create().expect("signal create for sleep");
    test_harness::check!(idle.wait(SIGNALED, budget).is_err());
}

extern "C" fn delayed_signaler(raw: usize) -> u32 {
    sleep(WORKER_DELAY);
    let handle = NonZeroU32::new(raw as u32)
        .map(Handle::<kind::Signal>::from_abi)
        .expect("signal handle id");
    u32::from(signal_set(handle, SIGNALED, 0, WakeCount::All) != 0)
}

#[test_harness::test]
fn wait_blocks_until_another_thread_signals() {
    let signal = Signal::create().expect("signal create must succeed");
    let raw = signal.as_raw().raw() as usize;

    let worker = spawn(delayed_signaler, raw).expect("spawn signaler");

    test_harness::check!(signal.wait(SIGNALED, Timeout::POLL).is_err());

    let observed = signal
        .wait(SIGNALED, WAKE_TIMEOUT)
        .expect("waiter must be woken by the signaler");
    test_harness::check!(observed & SIGNALED == SIGNALED);

    worker
        .join(WAKE_TIMEOUT)
        .expect("signaler joins before timeout");
}

#[test_harness::test]
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
    test_harness::check_eq!(index, 1);
}

#[test_harness::test]
fn wait_many_blocks_until_another_thread_signals() {
    let idle = Signal::create().expect("idle signal create");
    let target = Signal::create().expect("target signal create");
    let raw = target.as_raw().raw() as usize;

    let worker = spawn(delayed_signaler, raw).expect("spawn signaler");

    let items = [
        WaitItem::new(idle.as_raw(), SIGNALED),
        WaitItem::new(target.as_raw(), SIGNALED),
    ];
    test_harness::check!(matches!(
        wait_any(&items, Timeout::POLL),
        Err(kernel_objects::Error::Syscall(SyscallError::Timeout))
    ));

    let index = wait_any(&items, WAKE_TIMEOUT).expect("waiter must be woken by the signaler");
    test_harness::check_eq!(index, 1);

    worker
        .join(WAKE_TIMEOUT)
        .expect("signaler joins before timeout");
}

extern "C" fn delayed_closer(raw: usize) -> u32 {
    sleep(WORKER_DELAY);
    let Some(raw) = NonZeroU32::new(raw as u32) else {
        return 1;
    };
    // SAFETY: основной поток отказался от записи через `into_raw` и передал номер только этому
    // потоку.
    let origin: Signal = OwnedHandle::from(unsafe { IssuedHandle::adopt(raw) }.assume_kind());
    u32::from(origin.close().is_err())
}

fn wait_while_a_closer_revokes_the_derived() -> kernel_objects::Result<usize> {
    let own = Signal::create().expect("own signal create");
    let origin = Signal::create().expect("origin signal create");
    let derived = origin
        .duplicate(Rights::READ, None)
        .expect("derived signal duplicate");
    let raw = origin.into_raw().raw() as usize;

    let worker = spawn(delayed_closer, raw).expect("spawn closer");

    let items = [
        WaitItem::new(own.as_raw(), SIGNALED),
        WaitItem::new(derived.as_raw(), SIGNALED),
    ];
    let outcome = wait_any(&items, WAKE_TIMEOUT);

    test_harness::check!(worker.wait(WAKE_TIMEOUT).is_ok());
    test_harness::check_eq!(worker.exit_code(), Ok(0));
    test_harness::check!(worker.join(Timeout::POLL).is_ok());
    outcome
}

#[test_harness::test]
fn wait_many_is_canceled_when_a_derived_capability_is_revoked() {
    // Закрытие, опередившее начало ожидания, отвергает его кодом `Revoked`, и сценарий
    // повторяется с новыми объектами.
    let outcome = (0..REVOKE_RACE_ATTEMPTS)
        .map(|_| wait_while_a_closer_revokes_the_derived())
        .find(|outcome| *outcome != Err(kernel_objects::Error::Syscall(SyscallError::Revoked)));

    test_harness::check_eq!(
        outcome,
        Some(Err(kernel_objects::Error::Syscall(SyscallError::Canceled)))
    );
}

#[test_harness::test]
fn wait_many_rejects_a_set_outside_the_bound() {
    let signal = Signal::create().expect("signal create");
    let item = WaitItem::new(signal.as_raw(), SIGNALED);

    test_harness::check!(matches!(
        wait_any(&[], Timeout::POLL),
        Err(kernel_objects::Error::Syscall(
            SyscallError::InvalidArgument
        ))
    ));

    let oversized = [item; WAIT_MANY_MAX_COUNT + 1];
    test_harness::check!(matches!(
        wait_any(&oversized, Timeout::POLL),
        Err(kernel_objects::Error::Syscall(
            SyscallError::InvalidArgument
        ))
    ));
}
