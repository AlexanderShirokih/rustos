//! E2E проверка userspace `Mutex`/`Condvar` под контенцией из EL0:
//! рабочие потоки спавнятся через `spawn` в текущем процессе.

use core::{
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    time::Duration,
};

use kernel_objects::Signal;
use runtime::{Condvar, Instant, Mutex, RawCondvar, RawMutex, Timeout, sleep, spawn};
use syscall::{SIGNALED, WakeCount};

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

#[test_harness::test(bench)]
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
    let thread = spawn(pingpong_worker, arg).expect("spawn worker");

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
    test_harness::report!("sched thread_round_trip ns_per_op={}", elapsed / ROUNDS);
}

#[test_harness::test]
fn mutex_contention() {
    let shared = Shared { m: Mutex::new(0) };
    let arg = (&raw const shared) as usize;
    let threads: [_; WORKERS as usize] =
        core::array::from_fn(|_| spawn(contention_worker, arg).expect("spawn worker"));
    for thread in threads {
        thread
            .join(JOIN_TIMEOUT)
            .expect("worker joins before timeout");
    }
    test_harness::check_eq!(*shared.m.lock(), WORKERS * ITERS);
}

#[test_harness::test]
fn condvar_ping_pong() {
    let shared = Shared2 {
        m: Mutex::new(PingPong {
            turn: TURN_MAIN,
            count: 0,
        }),
        cv: Condvar::new(),
    };
    let arg = (&raw const shared) as usize;
    let thread = spawn(pingpong_worker, arg).expect("spawn worker");

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
    test_harness::check_eq!(shared.m.lock().count, 2 * ROUNDS);
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

#[test_harness::test]
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

    let victim = spawn(theft_victim, arg).expect("spawn victim");
    let thief = spawn(theft_thief, arg).expect("spawn thief");

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

#[test_harness::test]
fn condvar_notify_one_releases_waiters_one_by_one() {
    let shared = Baton {
        tokens: Mutex::new(0),
        condvar: Condvar::new(),
        registrations: AtomicU32::new(0),
        released: AtomicU32::new(0),
        progress: Signal::create().expect("create progress"),
    };
    let arg = (&raw const shared) as usize;

    let first = spawn(baton_waiter, arg).expect("spawn first");
    let second = spawn(baton_waiter, arg).expect("spawn second");

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

    test_harness::check!(released_in_turn);
}

struct Teardown {
    mutex: RawMutex,
    condvar: RawCondvar,
    in_window: Signal,
    notified: Signal,
    pause: Signal,
    destroyed: AtomicBool,
    destroyed_in_window: AtomicBool,
}

extern "C" fn teardown_waiter(arg: usize) -> u32 {
    // SAFETY: arg - адрес Teardown в кадре спавнящего потока, живой до завершения
    // join, который выполняется до выхода из кадра.
    let shared = unsafe { &*(arg as *const Teardown) };

    shared.mutex.lock();
    // SAFETY: поток держит `shared.mutex`.
    unsafe {
        shared.condvar.wait_interleaved(&shared.mutex, None, || {
            shared.in_window.notify_all().expect("report the window");
            let _ = shared.notified.wait(SIGNALED, JOIN_TIMEOUT);
            let _ = shared.pause.wait(SIGNALED, STEAL_WINDOW);
            shared
                .destroyed_in_window
                .store(shared.destroyed.load(Ordering::Acquire), Ordering::Release);
        });
    }

    // SAFETY: ожидание вернуло мьютекс захваченным.
    unsafe { shared.mutex.unlock() };
    0
}

// Ждущий читал слово счётчика после того, как `destroy` уже вернул управление.
#[test_harness::test]
fn raw_condvar_destroy_waits_for_notified_waiter() {
    let shared = Teardown {
        mutex: RawMutex::new(),
        condvar: RawCondvar::new(),
        in_window: Signal::create().expect("create in_window"),
        notified: Signal::create().expect("create notified"),
        pause: Signal::create().expect("create pause"),
        destroyed: AtomicBool::new(false),
        destroyed_in_window: AtomicBool::new(false),
    };
    let arg = (&raw const shared) as usize;
    let waiter = spawn(teardown_waiter, arg).expect("spawn waiter");

    shared
        .in_window
        .wait(SIGNALED, JOIN_TIMEOUT)
        .expect("waiter reaches the window");

    shared.condvar.notify_all();
    shared
        .notified
        .notify_all()
        .expect("report the notification");

    shared.condvar.destroy();
    shared.destroyed.store(true, Ordering::Release);

    waiter
        .join(JOIN_TIMEOUT)
        .expect("waiter joins before timeout");

    test_harness::check!(!shared.destroyed_in_window.load(Ordering::Acquire));
}

const SHORT_WAIT: Duration = Duration::from_millis(50);
const LONG_WAIT: Duration = Duration::from_secs(5);

const HOLD_PAUSE: Duration = Duration::from_millis(20);

struct Timed {
    mutex: RawMutex,
    condvar: RawCondvar,
    waiting: Signal,
    done: AtomicBool,
}

impl Timed {
    fn new() -> Self {
        Self {
            mutex: RawMutex::new(),
            condvar: RawCondvar::new(),
            waiting: Signal::create().expect("create waiting"),
            done: AtomicBool::new(false),
        }
    }
}

fn waited_at_least(start: Instant, duration: Duration) -> bool {
    u128::from(Instant::now().since(start).raw()) >= duration.as_nanos()
}

extern "C" fn timed_locker(arg: usize) -> u32 {
    // SAFETY: arg - адрес Timed в кадре спавнящего потока, живой до завершения
    // join, который выполняется до выхода из кадра.
    let shared = unsafe { &*(arg as *const Timed) };

    shared.waiting.notify_all().expect("report the wait");
    if shared.mutex.lock_timeout(LONG_WAIT) {
        shared.done.store(true, Ordering::Release);
        // SAFETY: мьютекс захвачен `lock_timeout` этим потоком.
        unsafe { shared.mutex.unlock() };
    }
    0
}

extern "C" fn timed_notifier(arg: usize) -> u32 {
    // SAFETY: см. timed_locker.
    let shared = unsafe { &*(arg as *const Timed) };

    shared.mutex.lock();
    shared.done.store(true, Ordering::Release);
    shared.condvar.notify_one();
    // SAFETY: мьютекс захвачен `lock` этим потоком.
    unsafe { shared.mutex.unlock() };
    0
}

#[test_harness::test]
fn raw_mutex_lock_timeout_expires_while_the_mutex_is_held() {
    let mutex = RawMutex::new();
    mutex.lock();

    let start = Instant::now();
    test_harness::check!(!mutex.lock_timeout(SHORT_WAIT));
    test_harness::check!(waited_at_least(start, SHORT_WAIT));
    test_harness::check!(!mutex.try_lock());

    // SAFETY: мьютекс захвачен `lock` этим потоком.
    unsafe { mutex.unlock() };
}

#[test_harness::test]
fn raw_mutex_lock_timeout_takes_the_mutex_released_before_the_deadline() {
    let shared = Timed::new();
    shared.mutex.lock();
    let arg = (&raw const shared) as usize;
    let locker = spawn(timed_locker, arg).expect("spawn locker");

    shared
        .waiting
        .wait(SIGNALED, JOIN_TIMEOUT)
        .expect("locker starts waiting");
    sleep(HOLD_PAUSE);
    // SAFETY: мьютекс захвачен `lock` этим потоком.
    unsafe { shared.mutex.unlock() };

    locker
        .join(JOIN_TIMEOUT)
        .expect("locker joins before timeout");
    test_harness::check!(shared.done.load(Ordering::Acquire));
}

#[test_harness::test]
fn raw_condvar_wait_timeout_expires_without_a_notification() {
    let shared = Timed::new();
    shared.mutex.lock();

    let start = Instant::now();
    // SAFETY: поток держит `shared.mutex`.
    let notified = unsafe { shared.condvar.wait_timeout(&shared.mutex, SHORT_WAIT) };
    test_harness::check!(!notified);
    test_harness::check!(waited_at_least(start, SHORT_WAIT));
    test_harness::check!(!shared.mutex.try_lock());

    // SAFETY: ожидание вернуло мьютекс захваченным.
    unsafe { shared.mutex.unlock() };
}

#[test_harness::test]
fn raw_condvar_wait_timeout_returns_on_a_notification_before_the_deadline() {
    let shared = Timed::new();
    shared.mutex.lock();
    let arg = (&raw const shared) as usize;
    let notifier = spawn(timed_notifier, arg).expect("spawn notifier");

    let start = Instant::now();
    let mut woken = true;
    while woken && !shared.done.load(Ordering::Acquire) {
        // SAFETY: поток держит `shared.mutex`.
        woken = unsafe { shared.condvar.wait_timeout(&shared.mutex, LONG_WAIT) };
    }
    test_harness::check!(woken);
    test_harness::check!(!waited_at_least(start, LONG_WAIT));

    // SAFETY: ожидание вернуло мьютекс захваченным.
    unsafe { shared.mutex.unlock() };
    notifier
        .join(JOIN_TIMEOUT)
        .expect("notifier joins before timeout");
}
