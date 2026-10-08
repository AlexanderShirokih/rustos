//! E2E проверка глобального heap процесса из EL0: small-path, passthrough и
//! многопоточная контенция на общей куче.

use alloc::{boxed::Box, string::String, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use runtime::{Priority, Timeout, spawn};

const STACK_SIZE: u64 = 0x4000;

const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);

#[test_harness::test]
fn heap_basic() {
    let mut v: Vec<u64> = Vec::new();
    for i in 0..1000 {
        v.push(i);
    }
    let sum: u64 = v.iter().copied().sum();
    test_harness::check_eq!(sum, 999 * 1000 / 2);

    let boxed = Box::new(0xDEAD_BEEF_u64);
    test_harness::check_eq!(*boxed, 0xDEAD_BEEF_u64);

    let mut s = String::new();
    for _ in 0..64 {
        s.push_str("ab");
    }
    test_harness::check_eq!(s.len(), 128);
}

#[test_harness::test]
fn heap_threshold_boundary() {
    for size in [64 * 1024 - 1, 64 * 1024, 64 * 1024 + 1] {
        let mut v: Vec<u8> = Vec::with_capacity(size);
        v.resize(size, 0xA5);
        test_harness::check_eq!(v.len(), size);
        test_harness::check_eq!(v[0], 0xA5);
        test_harness::check_eq!(v[size - 1], 0xA5);
    }
}

#[test_harness::test]
fn heap_passthrough() {
    const BIG: usize = 128 * 1024;
    for round in 0..8u8 {
        let mut v: Vec<u8> = Vec::with_capacity(BIG);
        v.resize(BIG, round);
        test_harness::check_eq!(v.len(), BIG);
        test_harness::check_eq!(v[0], round);
        test_harness::check_eq!(v[BIG - 1], round);
    }
}

const WORKERS: u64 = 4;
const ITERS: u64 = 200;
const VECTOR_LEN: u64 = 64;

struct ConcurrentShared {
    total: AtomicU64,
}

extern "C" fn churn_worker(arg: usize) -> u32 {
    // SAFETY: arg - адрес ConcurrentShared в кадре спавнящего потока, живой до
    // завершения join, который выполняется до выхода из кадра.
    let shared = unsafe { &*(arg as *const ConcurrentShared) };
    for _ in 0..ITERS {
        let mut v: Vec<u64> = Vec::with_capacity(VECTOR_LEN as usize);
        for i in 0..VECTOR_LEN {
            v.push(i);
        }
        let sum: u64 = v.iter().copied().sum();
        shared.total.fetch_add(sum, Relaxed);
    }
    0
}

#[test_harness::test]
fn heap_concurrent() {
    let shared = ConcurrentShared {
        total: AtomicU64::new(0),
    };
    let arg = (&raw const shared) as usize;
    let mut threads: Vec<_> = Vec::new();
    for _ in 0..WORKERS {
        threads.push(spawn(churn_worker, arg, STACK_SIZE, Priority::new(1)).expect("spawn worker"));
    }
    for thread in threads {
        thread
            .join(JOIN_TIMEOUT)
            .expect("worker joins before timeout");
    }

    let per_iter = VECTOR_LEN * (VECTOR_LEN - 1) / 2;
    let expected = WORKERS * ITERS * per_iter;
    test_harness::check_eq!(shared.total.load(Relaxed), expected);
}
