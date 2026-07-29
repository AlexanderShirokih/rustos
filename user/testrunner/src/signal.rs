//! E2E проверка `SignalCreate`/`SignalSet`/`SignalWaitOne` из EL0.

use kernel_tests::kernel_test;
use runtime::{Priority, Signal, Timeout, signal_set, spawn};
use syscall::{Handle, SIGNALED, WakeCount};

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
