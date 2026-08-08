//! E2E проверка userspace futex из EL0: ожидание по адресу, адресное
//! пробуждение и парковка под контенцией.

use alloc::{sync::Arc, vec::Vec};
use core::{
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use kernel_tests::kernel_test;
use runtime::{
    Futex, Instant, JoinHandle, Priority, Signal, Timeout, futex_bucket, futex_wait,
    futex_wait_interleaved, futex_wait_without_slot, futex_wake, futex_wake_all, spawn_owned,
    thread_yield,
};
use syscall::SIGNALED;

const STACK_SIZE: u64 = 0x4000;

const JOIN_TIMEOUT: Timeout = Timeout::from_secs(10);

const PARK_LIMIT: Option<Duration> = Some(Duration::from_secs(10));

const PARKED: u32 = 1;

const RELEASED: u32 = 2;

const SETTLE_MS: u64 = 100;

const OBSERVE_MS: u64 = 300;

const WAITERS: usize = 3;

const RUNNERS: u32 = 4;

const LAPS: u32 = 50;

const CELLS: usize = 16;

struct Pack {
    futex: Futex,
    ready: AtomicU32,
    returned: AtomicU32,
}

struct Handoff {
    futex: Futex,
    reached: Signal,
    wake_done: Signal,
    outcome: AtomicU32,
}

struct Colliding {
    cells: [Futex; CELLS],
    ready: AtomicU32,
    first_outcome: AtomicU32,
    second_outcome: AtomicU32,
}

struct Baton {
    turn: Futex,
    total: AtomicU32,
}

impl Pack {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            futex: Futex::new(PARKED),
            ready: AtomicU32::new(0),
            returned: AtomicU32::new(0),
        })
    }

    fn spawn_waiters(self: &Arc<Self>, count: usize) -> Vec<JoinHandle> {
        (0..count)
            .map(|_| {
                let pack = Arc::clone(self);
                spawn_owned(move || pack.wait_once(), STACK_SIZE, Priority::new(1))
                    .expect("spawn waiter")
            })
            .collect()
    }

    fn wait_once(&self) -> u32 {
        self.ready.fetch_add(1, Ordering::AcqRel);
        let released = futex_wait(&self.futex, PARKED, PARK_LIMIT);
        self.returned
            .fetch_add(u32::from(released), Ordering::AcqRel);
        0
    }

    fn release(&self) {
        self.futex.store(RELEASED, Ordering::Release);
        futex_wake_all(&self.futex);
    }
}

fn pause(millis: u64) {
    let idle = Signal::create().expect("create idle signal");
    let _ = idle.wait(SIGNALED, Timeout::from_millis(millis));
}

fn wait_for(counter: &AtomicU32, target: u32, limit: Timeout) -> bool {
    let started = Instant::now();
    while counter.load(Ordering::Acquire) < target {
        if Instant::now().since(started).raw() > limit.raw() {
            return false;
        }
        thread_yield();
    }
    true
}

fn join_all(threads: Vec<JoinHandle>) {
    for thread in threads {
        thread
            .join(JOIN_TIMEOUT)
            .expect("thread joins before timeout");
    }
}

#[kernel_test]
fn wait_returns_at_once_when_the_value_differs() {
    let futex = Futex::new(RELEASED);
    let started = Instant::now();

    kernel_tests::kassert!(futex_wait(&futex, PARKED, PARK_LIMIT));
    kernel_tests::kassert!(Instant::now().since(started).raw() < Timeout::from_secs(1).raw());
}

#[kernel_test]
fn wait_reports_the_timeout_it_was_given() {
    let futex = Futex::new(PARKED);
    let started = Instant::now();

    kernel_tests::kassert!(!futex_wait(&futex, PARKED, Some(Duration::from_millis(50))));
    kernel_tests::kassert!(Instant::now().since(started).raw() >= Timeout::from_millis(40).raw());
}

#[kernel_test]
fn wake_addresses_a_parked_waiter() {
    let pack = Pack::new();
    let waiters = pack.spawn_waiters(1);

    kernel_tests::kassert!(wait_for(&pack.ready, 1, JOIN_TIMEOUT));
    pause(SETTLE_MS);

    let addressed = futex_wake(&pack.futex);
    let released = wait_for(&pack.returned, 1, Timeout::from_secs(2));

    pack.release();
    join_all(waiters);

    kernel_tests::kassert!(addressed);
    kernel_tests::kassert!(released);
}

#[kernel_test]
fn wake_all_releases_every_waiter() {
    let pack = Pack::new();
    let waiters = pack.spawn_waiters(WAITERS);

    kernel_tests::kassert!(wait_for(&pack.ready, WAITERS as u32, JOIN_TIMEOUT));
    pause(SETTLE_MS);

    futex_wake_all(&pack.futex);
    let released_in_time = wait_for(&pack.returned, WAITERS as u32, Timeout::from_secs(2));

    pack.release();
    join_all(waiters);

    kernel_tests::kassert!(released_in_time);
}

// Regression guard: пробуждение, пришедшее между проверкой значения и
// парковкой, снимает парковку и не теряется на липком бите.
#[kernel_test]
fn wake_inside_the_park_window_is_not_lost() {
    let shared = Arc::new(Handoff {
        futex: Futex::new(PARKED),
        reached: Signal::create().expect("create reached"),
        wake_done: Signal::create().expect("create wake_done"),
        outcome: AtomicU32::new(0),
    });

    let waiter = {
        let shared = Arc::clone(&shared);
        spawn_owned(
            move || {
                let released = futex_wait_interleaved(&shared.futex, PARKED, PARK_LIMIT, || {
                    shared.reached.notify_all().expect("report the window");
                    shared
                        .wake_done
                        .wait(SIGNALED, JOIN_TIMEOUT)
                        .expect("wake lands before the park");
                });
                shared.outcome.store(u32::from(released), Ordering::Release);
                0
            },
            STACK_SIZE,
            Priority::new(1),
        )
        .expect("spawn waiter")
    };

    shared
        .reached
        .wait(SIGNALED, JOIN_TIMEOUT)
        .expect("waiter reaches the window");
    let addressed = futex_wake(&shared.futex);
    shared.wake_done.notify_all().expect("release the waiter");

    waiter
        .join(JOIN_TIMEOUT)
        .expect("waiter joins before timeout");

    kernel_tests::kassert!(addressed);
    kernel_tests::kassert_eq!(shared.outcome.load(Ordering::Acquire), 1);
}

#[kernel_test]
fn baton_under_contention_completes_every_lap() {
    let shared = Arc::new(Baton {
        turn: Futex::new(0),
        total: AtomicU32::new(0),
    });

    let runners: Vec<JoinHandle> = (0..RUNNERS)
        .map(|index| {
            let shared = Arc::clone(&shared);
            spawn_owned(
                move || {
                    for _ in 0..LAPS {
                        loop {
                            let current = shared.turn.load(Ordering::Acquire);
                            if current == index {
                                break;
                            }
                            futex_wait(&shared.turn, current, PARK_LIMIT);
                        }
                        shared.total.fetch_add(1, Ordering::AcqRel);
                        shared.turn.store((index + 1) % RUNNERS, Ordering::Release);
                        futex_wake_all(&shared.turn);
                    }
                    0
                },
                STACK_SIZE,
                Priority::new(1),
            )
            .expect("spawn runner")
        })
        .collect();

    join_all(runners);

    kernel_tests::kassert_eq!(shared.total.load(Ordering::Acquire), RUNNERS * LAPS);
    kernel_tests::kassert_eq!(shared.turn.load(Ordering::Acquire), 0);
}

#[kernel_test]
fn a_waiter_without_a_slot_follows_the_value() {
    let pack = Pack::new();
    let waiter = {
        let pack = Arc::clone(&pack);
        spawn_owned(
            move || {
                pack.ready.fetch_add(1, Ordering::AcqRel);
                let released = futex_wait_without_slot(&pack.futex, PARKED, PARK_LIMIT);
                pack.returned
                    .fetch_add(u32::from(released), Ordering::AcqRel);
                0
            },
            STACK_SIZE,
            Priority::new(1),
        )
        .expect("spawn waiter")
    };

    kernel_tests::kassert!(wait_for(&pack.ready, 1, JOIN_TIMEOUT));
    pause(SETTLE_MS);
    let parked_still = pack.returned.load(Ordering::Acquire);

    pack.release();
    waiter
        .join(JOIN_TIMEOUT)
        .expect("waiter joins before timeout");

    kernel_tests::kassert_eq!(parked_still, 0);
    kernel_tests::kassert_eq!(pack.returned.load(Ordering::Acquire), 1);
}

#[kernel_test]
fn a_waiter_without_a_slot_reports_its_timeout() {
    let futex = Futex::new(PARKED);
    let started = Instant::now();

    kernel_tests::kassert!(!futex_wait_without_slot(
        &futex,
        PARKED,
        Some(Duration::from_millis(50))
    ));
    kernel_tests::kassert!(Instant::now().since(started).raw() >= Timeout::from_millis(40).raw());
}

fn colliding_pair(cells: &[Futex]) -> Option<(usize, usize)> {
    for first in 0..cells.len() {
        for second in (first + 1)..cells.len() {
            if futex_bucket(&cells[first]) == futex_bucket(&cells[second]) {
                return Some((first, second));
            }
        }
    }
    None
}

// Regression guard: бакет общий, а пробуждение адресное - ждущий на соседнем
// адресе того же бакета остаётся в ожидании.
#[kernel_test]
fn a_wake_leaves_a_colliding_address_parked() {
    let shared = Arc::new(Colliding {
        cells: [const { Futex::new(PARKED) }; CELLS],
        ready: AtomicU32::new(0),
        first_outcome: AtomicU32::new(0),
        second_outcome: AtomicU32::new(0),
    });
    let (first, second) = colliding_pair(&shared.cells).expect("two of the cells share one bucket");

    let waiters: Vec<JoinHandle> = [(first, true), (second, false)]
        .into_iter()
        .map(|(cell, is_first)| {
            let shared = Arc::clone(&shared);
            spawn_owned(
                move || {
                    shared.ready.fetch_add(1, Ordering::AcqRel);
                    let released = futex_wait(&shared.cells[cell], PARKED, PARK_LIMIT);
                    let outcome = if is_first {
                        &shared.first_outcome
                    } else {
                        &shared.second_outcome
                    };
                    outcome.store(u32::from(released), Ordering::Release);
                    0
                },
                STACK_SIZE,
                Priority::new(1),
            )
            .expect("spawn waiter")
        })
        .collect();

    kernel_tests::kassert!(wait_for(&shared.ready, 2, JOIN_TIMEOUT));
    pause(SETTLE_MS);

    futex_wake(&shared.cells[first]);
    pause(OBSERVE_MS);
    let woken = shared.first_outcome.load(Ordering::Acquire);
    let untouched = shared.second_outcome.load(Ordering::Acquire);

    shared.cells[second].store(RELEASED, Ordering::Release);
    futex_wake_all(&shared.cells[second]);
    join_all(waiters);

    kernel_tests::kassert_eq!(woken, 1);
    kernel_tests::kassert_eq!(untouched, 0);
}
