//! E2E проверка глобального heap процесса из EL0: small-path, passthrough и
//! многопоточная контенция на общей куче.

use alloc::{boxed::Box, string::String, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use kernel_tests::kernel_test;
use runtime::{Priority, Timeout, spawn};

/// Размер стека рабочего потока (page-aligned выдача -> вершина 16-байт-выровнена).
const STACK_SIZE: u64 = 0x4000;

/// Таймаут join'а: конечный, чтобы зависший worker падал по timeout.
const JOIN_TIMEOUT_NS: u64 = 5_000_000_000;

#[kernel_test]
fn heap_basic() {
    let mut v: Vec<u64> = Vec::new();
    for i in 0..1000 {
        v.push(i);
    }
    let sum: u64 = v.iter().copied().sum();
    kernel_tests::kassert_eq!(sum, 999 * 1000 / 2);

    let boxed = Box::new(0xDEAD_BEEF_u64);
    kernel_tests::kassert_eq!(*boxed, 0xDEAD_BEEF_u64);

    let mut s = String::new();
    for _ in 0..64 {
        s.push_str("ab");
    }
    kernel_tests::kassert_eq!(s.len(), 128);
}

#[kernel_test]
fn heap_threshold_boundary() {
    for size in [64 * 1024 - 1, 64 * 1024, 64 * 1024 + 1] {
        let mut v: Vec<u8> = Vec::with_capacity(size);
        v.resize(size, 0xA5);
        kernel_tests::kassert_eq!(v.len(), size);
        kernel_tests::kassert_eq!(v[0], 0xA5);
        kernel_tests::kassert_eq!(v[size - 1], 0xA5);
    }
}

#[kernel_test]
fn heap_passthrough() {
    const BIG: usize = 128 * 1024;
    for round in 0..8u8 {
        let mut v: Vec<u8> = Vec::with_capacity(BIG);
        v.resize(BIG, round);
        kernel_tests::kassert_eq!(v.len(), BIG);
        kernel_tests::kassert_eq!(v[0], round);
        kernel_tests::kassert_eq!(v[BIG - 1], round);
    }
}

/// Число рабочих потоков и итераций в `heap_concurrent`.
const WORKERS: u64 = 4;
const ITERS: u64 = 200;
/// Длина рабочего вектора на итерацию.
const LEN: u64 = 64;

struct ConcurrentShared {
    total: AtomicU64,
}

extern "C" fn churn_worker(arg: usize) -> u32 {
    // SAFETY: arg - адрес ConcurrentShared в кадре спавнящего потока; жив до
    // join'а, который выполняется до выхода из кадра.
    let shared = unsafe { &*(arg as *const ConcurrentShared) };
    for _ in 0..ITERS {
        let mut v: Vec<u64> = Vec::with_capacity(LEN as usize);
        for i in 0..LEN {
            v.push(i);
        }
        let sum: u64 = v.iter().copied().sum();
        shared.total.fetch_add(sum, Relaxed);
    }
    0
}

#[kernel_test]
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
            .join(Timeout::from_ns(JOIN_TIMEOUT_NS))
            .expect("worker joins before timeout");
    }

    let per_iter = LEN * (LEN - 1) / 2;
    let expected = WORKERS * ITERS * per_iter;
    kernel_tests::kassert_eq!(shared.total.load(Relaxed), expected);
}
