//! E2E уступки кванта из EL0.

use runtime::{Timeout, spawn, thread_yield};

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
    let worker = spawn(yielding_worker, 0).expect("spawn worker");

    worker
        .join(JOIN_TIMEOUT)
        .expect("worker joins before timeout");
}
