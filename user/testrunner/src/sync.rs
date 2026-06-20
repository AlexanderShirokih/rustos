//! E2E проверка userspace `Mutex`/`Condvar` под контенцией из EL0:
//! рабочие потоки спавнятся через `thread_create` в текущем процессе.

use kernel_tests::kernel_test;
use runtime::{
    Condvar, Mutex, memory_allocate, process_self, signal_wait_one, thread_create, thread_exit,
    thread_termination_signal,
};
use syscall::{Handle, MEM_FLAGS_READ_WRITE, SIGNALED};

/// Размер стека рабочего потока (page-aligned выдача memory_allocate -> вершина
/// 16-байт-выровнена).
const STACK_SIZE: u64 = 0x4000;

/// Таймаут join'а: конечный, чтобы зависший worker падал по timeout.
const JOIN_TIMEOUT_NS: u64 = 5_000_000_000;

struct Shared {
    m: Mutex<u64>,
}

/// Состояние ping-pong: чей ход и общий счётчик инкрементов.
struct PingPong {
    turn: u32,
    count: u64,
}

struct Shared2 {
    m: Mutex<PingPong>,
    cv: Condvar,
}

/// Число рабочих потоков в `mutex_contention`.
const WORKERS: u64 = 2;
/// Инкрементов на поток.
const ITERS: u64 = 500;

/// Раундов ping-pong на каждую сторону в `condvar_ping_pong`.
const ROUNDS: u64 = 50;
const TURN_MAIN: u32 = 0;
const TURN_WORKER: u32 = 1;

/// Спавнит worker в текущем процессе: выделяет стек и стартует поток с
/// `arg` через `thread_create`. Возвращает handle потока для join'а.
fn spawn(worker: extern "C" fn(usize) -> !, arg: usize) -> Handle {
    let va = memory_allocate(STACK_SIZE, MEM_FLAGS_READ_WRITE);
    kernel_tests::kassert!(va > 0);
    let user_sp = u64::try_from(va).expect("positive va fits u64") + STACK_SIZE;
    let process = process_self().expect("process_self handle");
    thread_create(process, worker as usize as u64, user_sp, arg as u64, 1)
        .expect("thread_create handle")
}

/// Ждёт завершения `thread` с конечным таймаутом.
fn join(thread: Handle) {
    let sig = thread_termination_signal(thread).expect("thread_termination_signal");
    let observed = signal_wait_one(sig, SIGNALED, JOIN_TIMEOUT_NS);
    kernel_tests::kassert_eq!(observed, i64::from(SIGNALED));
}

extern "C" fn contention_worker(arg: usize) -> ! {
    // SAFETY: arg - адрес Shared в кадре спавнящего потока; жив до join'а,
    // который выполняется до выхода из кадра.
    let s = unsafe { &*(arg as *const Shared) };
    for _ in 0..ITERS {
        *s.m.lock() += 1;
    }
    thread_exit(0)
}

extern "C" fn pingpong_worker(arg: usize) -> ! {
    // SAFETY: arg - адрес Shared2 в кадре спавнящего потока; жив до join'а,
    // который выполняется до выхода из кадра.
    let s = unsafe { &*(arg as *const Shared2) };
    for _ in 0..ROUNDS {
        let mut g = s.m.lock();
        while g.turn != TURN_WORKER {
            g = s.cv.wait(g);
        }
        g.count += 1;
        g.turn = TURN_MAIN;
        s.cv.notify_all();
    }
    thread_exit(0)
}

#[kernel_test]
fn mutex_contention() {
    let shared = Shared { m: Mutex::new(0) };
    let arg = (&raw const shared) as usize;
    let mut threads = [None; WORKERS as usize];
    for slot in &mut threads {
        *slot = Some(spawn(contention_worker, arg));
    }
    for slot in &threads {
        join(slot.expect("thread handle"));
    }
    kernel_tests::kassert_eq!(*shared.m.lock(), WORKERS * ITERS);
}

#[kernel_test]
fn condvar_ping_pong() {
    let shared = Shared2 {
        m: Mutex::new(PingPong {
            turn: TURN_MAIN,
            count: 0,
        }),
        cv: Condvar::new(),
    };
    let arg = (&raw const shared) as usize;
    let thread = spawn(pingpong_worker, arg);

    for _ in 0..ROUNDS {
        let mut g = shared.m.lock();
        while g.turn != TURN_MAIN {
            g = shared.cv.wait(g);
        }
        g.count += 1;
        g.turn = TURN_WORKER;
        shared.cv.notify_all();
    }

    join(thread);
    kernel_tests::kassert_eq!(shared.m.lock().count, 2 * ROUNDS);
}
