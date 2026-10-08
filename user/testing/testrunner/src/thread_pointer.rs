//! E2E приватности TPIDR_EL0: значение потока сохраняется через переключение контекста и
//! соседним потокам недоступно.

use core::{
    arch::asm,
    sync::atomic::{AtomicU32, Ordering},
};

use runtime::{Priority, Timeout, spawn, thread_yield};

const WORKER_STACK_SIZE: u64 = 0x4000;
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const MAIN_POINTER: u64 = 0xA5A5_5A5A_0000_0001;
const WORKER_POINTER_BASE: u64 = 0x5A5A_A5A5_0000_0010;
const WORKERS: usize = 3;
const RECHECK_ROUNDS: u32 = 64;

struct Shared {
    next_worker_index: AtomicU32,
    mismatches: AtomicU32,
}

fn thread_pointer() -> u64 {
    let value: u64;
    // SAFETY: TPIDR_EL0 читается на EL0 по архитектуре и принадлежит текущему потоку; чтение
    // не трогает память.
    unsafe {
        asm!(
            "mrs {value}, tpidr_el0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
}

fn set_thread_pointer(value: u64) {
    // SAFETY: TPIDR_EL0 пишется на EL0 по архитектуре и принадлежит текущему потоку; запись
    // не трогает память. Локальными данными потока эта программа не пользуется, поэтому
    // подмена наблюдаема только тестом.
    unsafe {
        asm!(
            "msr tpidr_el0, {value}",
            value = in(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

extern "C" fn pointer_worker(arg: usize) -> u32 {
    // SAFETY: arg - адрес Shared в кадре спавнящего потока; он жив до присоединения рабочих
    // потоков, которое идёт до выхода из кадра.
    let shared = unsafe { &*(arg as *const Shared) };

    if thread_pointer() != 0 {
        shared.mismatches.fetch_add(1, Ordering::Relaxed);
    }

    let index = shared.next_worker_index.fetch_add(1, Ordering::Relaxed);
    let expected = WORKER_POINTER_BASE + u64::from(index);
    set_thread_pointer(expected);

    for _ in 0..RECHECK_ROUNDS {
        thread_yield();
        if thread_pointer() != expected {
            shared.mismatches.fetch_add(1, Ordering::Relaxed);
        }
    }
    // Регистр принадлежит локальным данным потока, а тест занял его под свою метку. Уход
    // потока разбирает эти данные по указателю из регистра, поэтому метку надо снять.
    set_thread_pointer(0);
    0
}

#[test_harness::test]
fn thread_pointer_is_private_to_the_thread() {
    let entry_pointer = thread_pointer();
    let shared = Shared {
        next_worker_index: AtomicU32::new(0),
        mismatches: AtomicU32::new(0),
    };
    let arg = (&raw const shared) as usize;

    set_thread_pointer(MAIN_POINTER);
    let threads: [_; WORKERS] = core::array::from_fn(|_| {
        spawn(pointer_worker, arg, WORKER_STACK_SIZE, Priority::new(1)).expect("spawn worker")
    });

    let mut main_mismatches = 0u32;
    for _ in 0..RECHECK_ROUNDS {
        thread_yield();
        if thread_pointer() != MAIN_POINTER {
            main_mismatches += 1;
        }
    }

    for thread in threads {
        thread
            .join(JOIN_TIMEOUT)
            .expect("worker joins before timeout");
    }
    set_thread_pointer(entry_pointer);

    test_harness::check_eq!(main_mismatches, 0);
    test_harness::check_eq!(shared.mismatches.load(Ordering::Relaxed), 0);
}
