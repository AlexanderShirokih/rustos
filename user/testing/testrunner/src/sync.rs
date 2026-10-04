//! E2E проверка userspace `Mutex`/`Condvar` под контенцией из EL0:
//! рабочие потоки спавнятся через `spawn` в текущем процессе.

use core::sync::atomic::{AtomicU32, Ordering};

use kernel_objects::Signal;
use kernel_tests::kernel_test;
use runtime::{Condvar, Instant, Mutex, Priority, Timeout, spawn};
use syscall::{SIGNALED, WakeCount};

const STACK_SIZE: u64 = 0x4000;

const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);

struct Shared {
    m: Mutex<u64>,
}

struct PingPong {
    turn: u32,
    count: u64,
}

struct Shared2 {
    m: Mutex<PingPong>,
    cv: Condvar,
}

const WORKERS: u64 = 2;
const ITERS: u64 = 500;

const ROUNDS: u64 = 50;
const TURN_MAIN: u32 = 0;
const TURN_WORKER: u32 = 1;

extern "C" fn contention_worker(arg: usize) -> u32 {
    // SAFETY: arg - адрес Shared в кадре спавнящего потока, живой до завершения
    // join, который выполняется до выхода из кадра.
    let s = unsafe { &*(arg as *const Shared) };
    for _ in 0..ITERS {
        *s.m.lock() += 1;
    }
    0
}

extern "C" fn pingpong_worker(arg: usize) -> u32 {
    // SAFETY: arg - адрес Shared2 в кадре спавнящего потока, живой до завершения
    // join, который выполняется до выхода из кадра.
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

#[kernel_test(bench)]
fn bench_thread_round_trip() {
    // Раунд между потоками одного процесса: переключение без смены адресного пространства.
    let shared = Shared2 {
        m: Mutex::new(PingPong {
            turn: TURN_MAIN,
            count: 0,
        }),
        cv: Condvar::new(),
    };
    let arg = (&raw const shared) as usize;
    let thread = spawn(pingpong_worker, arg, STACK_SIZE, Priority::new(1)).expect("spawn worker");

    let start = Instant::now();
    for _ in 0..ROUNDS {
        let mut g = shared.m.lock();
        while g.turn != TURN_MAIN {
            g = shared.cv.wait(g);
        }
        g.count += 1;
        g.turn = TURN_WORKER;
        shared.cv.notify_all();
    }
    let elapsed = Instant::now().as_nanos().saturating_sub(start.as_nanos());

    thread
        .join(JOIN_TIMEOUT)
        .expect("worker joins before timeout");
    kernel_tests::kreport!("sched thread_round_trip ns_per_op={}", elapsed / ROUNDS);
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
            .join(JOIN_TIMEOUT)
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
        .join(JOIN_TIMEOUT)
        .expect("worker joins before timeout");
    kernel_tests::kassert_eq!(shared.m.lock().count, 2 * ROUNDS);
}

const STEAL_WINDOW: Timeout = Timeout::from_millis(50);

struct Turns {
    victim_wake: bool,
    thief_wake: bool,
}

struct Theft {
    turns: Mutex<Turns>,
    condvar: Condvar,
    in_window: Signal,
    stolen: Signal,
    thief_release: Signal,
}

extern "C" fn theft_victim(arg: usize) -> u32 {
    // SAFETY: arg - адрес Theft в кадре спавнящего потока, живой до завершения
    // join, который выполняется до выхода из кадра.
    let shared = unsafe { &*(arg as *const Theft) };
    let mut guard = shared.turns.lock();
    while !guard.victim_wake {
        guard = shared.condvar.wait_interleaved(guard, || {
            shared.in_window.notify_all().expect("report the window");
            let _ = shared.stolen.wait(SIGNALED, STEAL_WINDOW);
        });
    }
    0
}

extern "C" fn theft_thief(arg: usize) -> u32 {
    // SAFETY: см. theft_victim.
    let shared = unsafe { &*(arg as *const Theft) };
    let _ = shared.thief_release.wait(SIGNALED, JOIN_TIMEOUT);

    let mut guard = shared.turns.lock();
    while !guard.thief_wake {
        guard = shared.condvar.wait(guard);
        shared.stolen.notify_all().expect("report the round");
    }
    0
}

#[kernel_test]
fn condvar_notify_is_not_stolen_by_a_later_waiter() {
    let shared = Theft {
        turns: Mutex::new(Turns {
            victim_wake: false,
            thief_wake: false,
        }),
        condvar: Condvar::new(),
        in_window: Signal::create().expect("create in_window"),
        stolen: Signal::create().expect("create stolen"),
        thief_release: Signal::create().expect("create thief_release"),
    };
    let arg = (&raw const shared) as usize;

    let victim = spawn(theft_victim, arg, STACK_SIZE, Priority::new(1)).expect("spawn victim");
    let thief = spawn(theft_thief, arg, STACK_SIZE, Priority::new(1)).expect("spawn thief");

    shared
        .in_window
        .wait(SIGNALED, JOIN_TIMEOUT)
        .expect("victim reaches the window");

    {
        let mut guard = shared.turns.lock();
        guard.victim_wake = true;
        shared.condvar.notify_all();
    }
    shared
        .thief_release
        .notify_all()
        .expect("release the thief");

    let victim_joined = victim.join(JOIN_TIMEOUT);

    {
        let mut guard = shared.turns.lock();
        guard.thief_wake = true;
        shared.condvar.notify_all();
    }
    thief
        .join(JOIN_TIMEOUT)
        .expect("thief joins before timeout");

    victim_joined.expect("victim joins before timeout");
}

struct Baton {
    tokens: Mutex<u32>,
    condvar: Condvar,
    registrations: AtomicU32,
    released: AtomicU32,
    progress: Signal,
}

fn wait_for_count(progress: &Signal, counter: &AtomicU32, target: u32) -> bool {
    while counter.load(Ordering::Acquire) < target {
        if progress.wait(SIGNALED, JOIN_TIMEOUT).is_err() {
            return false;
        }
        if progress.set(0, SIGNALED, WakeCount::None).is_err() {
            return false;
        }
    }
    true
}

extern "C" fn baton_waiter(arg: usize) -> u32 {
    // SAFETY: arg - адрес Baton в кадре спавнящего потока, живой до завершения
    // join, который выполняется до выхода из кадра.
    let shared = unsafe { &*(arg as *const Baton) };

    let mut guard = shared.tokens.lock();
    while *guard == 0 {
        guard = shared.condvar.wait_interleaved(guard, || {
            shared.registrations.fetch_add(1, Ordering::AcqRel);
            shared.progress.notify_all().expect("report registration");
        });
    }
    *guard -= 1;
    drop(guard);

    shared.released.fetch_add(1, Ordering::AcqRel);
    shared.progress.notify_all().expect("report release");
    0
}

#[kernel_test]
fn condvar_notify_one_releases_waiters_one_by_one() {
    let shared = Baton {
        tokens: Mutex::new(0),
        condvar: Condvar::new(),
        registrations: AtomicU32::new(0),
        released: AtomicU32::new(0),
        progress: Signal::create().expect("create progress"),
    };
    let arg = (&raw const shared) as usize;

    let first = spawn(baton_waiter, arg, STACK_SIZE, Priority::new(1)).expect("spawn first");
    let second = spawn(baton_waiter, arg, STACK_SIZE, Priority::new(1)).expect("spawn second");

    let mut released_in_turn = wait_for_count(&shared.progress, &shared.registrations, 2);

    let mut released = 0;
    while released_in_turn && released < 2 {
        released += 1;
        {
            let mut guard = shared.tokens.lock();
            *guard += 1;
            shared.condvar.notify_one();
        }
        released_in_turn = wait_for_count(&shared.progress, &shared.released, released);
    }

    {
        let mut guard = shared.tokens.lock();
        *guard += 2;
        shared.condvar.notify_all();
    }
    first
        .join(JOIN_TIMEOUT)
        .expect("first joins before timeout");
    second
        .join(JOIN_TIMEOUT)
        .expect("second joins before timeout");

    kernel_tests::kassert!(released_in_turn);
}
