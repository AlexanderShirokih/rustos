//! E2E проверка userspace `Mutex`/`Condvar` под контенцией из EL0:
//! рабочие потоки спавнятся через `spawn` в текущем процессе.

use kernel_tests::kernel_test;
use runtime::{Condvar, Mutex, Priority, Timeout, spawn};

/// Размер стека рабочего потока (page-aligned выдача -> вершина 16-байт-выровнена).
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

extern "C" fn contention_worker(arg: usize) -> u32 {
    // SAFETY: arg - адрес Shared в кадре спавнящего потока; жив до join'а,
    // который выполняется до выхода из кадра.
    let s = unsafe { &*(arg as *const Shared) };
    for _ in 0..ITERS {
        *s.m.lock() += 1;
    }
    0
}

extern "C" fn pingpong_worker(arg: usize) -> u32 {
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
    0
}

#[kernel_test]
fn mutex_contention() {
    let shared = Shared { m: Mutex::new(0) };
    let arg = (&raw const shared) as usize;
    let threads: [_; WORKERS as usize] = core::array::from_fn(|_| {
        spawn(contention_worker, arg, STACK_SIZE, Priority::new(1)).expect("spawn worker")
    });
    for thread in threads {
        thread
            .join(Timeout::from_ns(JOIN_TIMEOUT_NS))
            .expect("worker joins before timeout");
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
    let thread = spawn(pingpong_worker, arg, STACK_SIZE, Priority::new(1)).expect("spawn worker");

    for _ in 0..ROUNDS {
        let mut g = shared.m.lock();
        while g.turn != TURN_MAIN {
            g = shared.cv.wait(g);
        }
        g.count += 1;
        g.turn = TURN_WORKER;
        shared.cv.notify_all();
    }

    thread
        .join(Timeout::from_ns(JOIN_TIMEOUT_NS))
        .expect("worker joins before timeout");
    kernel_tests::kassert_eq!(shared.m.lock().count, 2 * ROUNDS);
}
