//! Ёмкость таблицы потоков в сборке ядра.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use kernelspace::syscall_bridge;
use scheduler::{SchedulerServiceExt, SpawnConfig, SpawnError};

use super::smp::wait_until;

const HELD_THREADS_ABOVE: usize = 64;

const HOLD_STEP_MS: u64 = 10;

struct Hold {
    released: AtomicBool,
    finished: AtomicUsize,
}

impl Hold {
    fn keep(&self) {
        let scheduler = syscall_bridge::scheduler().expect("scheduler installed");
        while !self.released.load(Ordering::Acquire) {
            scheduler.sleep_ms(HOLD_STEP_MS);
        }
        self.finished.fetch_add(1, Ordering::Release);
    }
}

#[test_harness::test]
fn the_thread_table_holds_more_than_64_threads_and_refuses_beyond() {
    let scheduler = syscall_bridge::scheduler().expect("scheduler installed");
    let hold = Arc::new(Hold {
        released: AtomicBool::new(false),
        finished: AtomicUsize::new(0),
    });

    let mut spawned = 0;
    let refusal = loop {
        let holder = hold.clone();
        match scheduler.spawn(SpawnConfig::new("table-fill"), move || holder.keep()) {
            Ok(_) => spawned += 1,
            Err(error) => break error,
        }
    };

    hold.released.store(true, Ordering::Release);
    assert!(
        wait_until(|| hold.finished.load(Ordering::Acquire) == spawned),
        "every holding thread must finish after the release"
    );

    assert!(
        matches!(refusal, SpawnError::NoFreeThreadSlots),
        "a full table must refuse with NoFreeThreadSlots, got {refusal:?}"
    );
    assert!(
        spawned > HELD_THREADS_ABOVE,
        "the table must hold more than {HELD_THREADS_ABOVE} threads, took {spawned}"
    );

    // Слот потока освобождается после того, как его отпустят все CPU.
    let done = Arc::new(AtomicBool::new(false));
    assert!(
        wait_until(|| {
            let done = done.clone();
            scheduler
                .spawn(SpawnConfig::new("table-reuse"), move || {
                    done.store(true, Ordering::Release);
                })
                .is_ok()
        }),
        "a finished thread must free its slot"
    );
    assert!(
        wait_until(|| done.load(Ordering::Acquire)),
        "the thread in a freed slot must run"
    );
}
