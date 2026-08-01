//! E2E проверка `SignalCreate`/`SignalSet`/`SignalWaitOne`/`SignalWaitMany`
//! из EL0.

use kernel_tests::kernel_test;
use runtime::{Priority, Signal, Timeout, signal_set, spawn, wait_any};
use syscall::{Handle, SIGNALED, SyscallError, WAIT_MANY_MAX_COUNT, WaitItem, WakeCount};

/// Размер стека рабочего потока.
const STACK_SIZE: u64 = 0x4000;
/// Бюджет ожидания пробуждения: конечный, чтобы потерянное пробуждение падало
/// по тайм-ауту, а не вешало прогон.
const WAKE_TIMEOUT_NS: u64 = 5_000_000_000;
/// Задержка воркера перед сигналом: за это время ожидающий гарантированно
/// успевает заблокироваться.
const WORKER_DELAY_NS: u64 = 50_000_000;

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

/// Приостанавливает поток на `ns`: ждём на собственном сигнале бит, который
/// никто не поднимает, и выходим по тайм-ауту.
fn sleep_ns(ns: u64) {
    let idle = Signal::create().expect("signal create for sleep");
    kernel_tests::kassert!(idle.wait(SIGNALED, Timeout::from_ns(ns)).is_err());
}

/// Воркер: выжидает, затем поднимает `SIGNALED` на сигнале, чей сырой
/// HandleId приехал аргументом.
extern "C" fn delayed_signaler(raw: usize) -> u32 {
    sleep_ns(WORKER_DELAY_NS);
    let handle = Handle::new(raw as u32).expect("signal handle id");
    u32::from(signal_set(handle, SIGNALED, 0, WakeCount::All) != 0)
}

/// Ожидающий блокируется в `SignalWaitOne` и просыпается сигналом, поднятым
/// другим потоком уже после блокировки.
#[kernel_test]
fn wait_blocks_until_another_thread_signals() {
    let signal = Signal::create().expect("signal create must succeed");
    let raw = signal.handle().as_raw().raw() as usize;

    let worker =
        spawn(delayed_signaler, raw, STACK_SIZE, Priority::new(1)).expect("spawn signaler");

    // Бит ещё не поднят: ожидание уйдёт в блокировку, а не вернётся сразу.
    kernel_tests::kassert!(signal.wait(SIGNALED, Timeout::POLL).is_err());

    let observed = signal
        .wait(SIGNALED, Timeout::from_ns(WAKE_TIMEOUT_NS))
        .expect("waiter must be woken by the signaler");
    kernel_tests::kassert!(observed & SIGNALED == SIGNALED);

    worker
        .join(Timeout::from_ns(WAKE_TIMEOUT_NS))
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
        WaitItem::new(idle.handle().as_raw(), SIGNALED),
        WaitItem::new(raised.handle().as_raw(), SIGNALED),
    ];
    let index = wait_any(&items, Timeout::POLL).expect("raised entry satisfies the wait");
    kernel_tests::kassert_eq!(index, 1);
}

#[kernel_test]
fn wait_many_blocks_until_another_thread_signals() {
    let idle = Signal::create().expect("idle signal create");
    let target = Signal::create().expect("target signal create");
    let raw = target.handle().as_raw().raw() as usize;

    let worker =
        spawn(delayed_signaler, raw, STACK_SIZE, Priority::new(1)).expect("spawn signaler");

    let items = [
        WaitItem::new(idle.handle().as_raw(), SIGNALED),
        WaitItem::new(target.handle().as_raw(), SIGNALED),
    ];
    // Биты ещё не подняты: ожидание уходит в блокировку вместо немедленного возврата.
    kernel_tests::kassert!(matches!(
        wait_any(&items, Timeout::POLL),
        Err(runtime::Error::Syscall(SyscallError::Timeout))
    ));

    let index = wait_any(&items, Timeout::from_ns(WAKE_TIMEOUT_NS))
        .expect("waiter must be woken by the signaler");
    kernel_tests::kassert_eq!(index, 1);

    worker
        .join(Timeout::from_ns(WAKE_TIMEOUT_NS))
        .expect("signaler joins before timeout");
}

#[kernel_test]
fn wait_many_rejects_a_set_outside_the_bound() {
    let signal = Signal::create().expect("signal create");
    let item = WaitItem::new(signal.handle().as_raw(), SIGNALED);

    kernel_tests::kassert!(matches!(
        wait_any(&[], Timeout::POLL),
        Err(runtime::Error::Syscall(SyscallError::InvalidArgument))
    ));

    let oversized = [item; WAIT_MANY_MAX_COUNT + 1];
    kernel_tests::kassert!(matches!(
        wait_any(&oversized, Timeout::POLL),
        Err(runtime::Error::Syscall(SyscallError::InvalidArgument))
    ));
}
