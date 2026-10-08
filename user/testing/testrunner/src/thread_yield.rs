//! E2E уступки кванта из EL0.

use runtime::{Priority, Timeout, spawn, thread_yield};

const WORKER_STACK_SIZE: u64 = 0x4000;
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const YIELD_ROUNDS: u32 = 256;

extern "C" fn yielding_worker(_arg: usize) -> u32 {
    for _ in 0..YIELD_ROUNDS {
        thread_yield();
    }
    0
}

#[test_harness::test]
fn yielded_thread_returns_to_the_cpu() {
    let worker =
        spawn(yielding_worker, 0, WORKER_STACK_SIZE, Priority::new(1)).expect("spawn worker");

    worker
        .join(JOIN_TIMEOUT)
        .expect("worker joins before timeout");
}
