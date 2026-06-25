//! E2E проверка глобального heap процесса из EL0: small-path, passthrough и
//! многопоточная контенция на общей куче.

use alloc::{boxed::Box, string::String, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use kernel_tests::kernel_test;
use runtime::{
    memory_allocate, process_resource_self, process_self, signal_wait_one, thread_create,
    thread_exit,
};
use syscall::{Handle, MEM_FLAGS_READ_WRITE, SIGNALED};

/// Размер стека рабочего потока (page-aligned выдача memory_allocate -> вершина
/// 16-байт-выровнена).
const STACK_SIZE: u64 = 0x4000;

/// Таймаут join'а: конечный, чтобы зависший worker падал по timeout.
const JOIN_TIMEOUT_NS: u64 = 5_000_000_000;

/// Спавнит worker в текущем процессе: выделяет стек и стартует поток с `arg`.
fn spawn(worker: extern "C" fn(usize) -> !, arg: usize) -> Handle {
    let resource = process_resource_self().expect("metering resource handle");
    let va = memory_allocate(resource, STACK_SIZE, MEM_FLAGS_READ_WRITE);
    kernel_tests::kassert!(va > 0);
    let user_sp = u64::try_from(va).expect("positive va fits u64") + STACK_SIZE;
    let process = process_self().expect("process_self handle");
    thread_create(process, worker as usize as u64, user_sp, arg as u64, 1)
        .expect("thread_create handle")
}

/// Ждёт завершения `thread` с конечным таймаутом (ожидание по thread-handle).
fn join(thread: Handle) {
    let observed = signal_wait_one(thread, SIGNALED, JOIN_TIMEOUT_NS);
    kernel_tests::kassert_eq!(observed, i64::from(SIGNALED));
}

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

/// На границе passthrough-порога маршрут alloc/dealloc по `>=` обязан совпасть:
/// round-trip ровно на пороге, на единицу ниже и выше.
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

/// Аллокации >= passthrough-порога идут напрямую к ядру и возвращаются на drop;
/// повтор проверяет переиспользование возвращённой памяти.
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

extern "C" fn churn_worker(arg: usize) -> ! {
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
    thread_exit(0)
}

/// Потоки одновременно аллоцируют и освобождают на общей куче; checksum ловит
/// потерянные итерации и порчу данных под контенцией на Mutex аллокатора.
#[kernel_test]
fn heap_concurrent() {
    let shared = ConcurrentShared {
        total: AtomicU64::new(0),
    };
    let arg = (&raw const shared) as usize;
    let mut threads = [None; WORKERS as usize];
    for slot in &mut threads {
        *slot = Some(spawn(churn_worker, arg));
    }
    for slot in &threads {
        join(slot.expect("thread handle"));
    }

    let per_iter = LEN * (LEN - 1) / 2;
    let expected = WORKERS * ITERS * per_iter;
    kernel_tests::kassert_eq!(shared.total.load(Relaxed), expected);
}
