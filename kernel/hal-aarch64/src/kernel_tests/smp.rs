//! Проводка планировщика на нескольких CPU.
//!
//! Выбор CPU и учёт очередей покрыты host-тестами планировщика; здесь
//! проверяется то, что на host не воспроизводится: потоки действительно
//! исполняются на разных ядрах, а поднятый CPU обслуживает их сам.

use alloc::sync::Arc;
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use kernel_tests::kernel_test;
use kernelspace::syscall_bridge;
use scheduler::{ArchCpu, SchedulerServiceExt, SpawnConfig};

use crate::{sched::Aarch64Cpu, smp};

const BARRIER_SPINS: usize = 50_000_000;

const WAIT_STEP_MS: u64 = 1;
const WAIT_STEPS: usize = 2_000;

struct Run {
    cpus: AtomicU32,
    arrived: AtomicUsize,
    done: AtomicUsize,
}

impl Run {
    fn new() -> Self {
        Self {
            cpus: AtomicU32::new(0),
            arrived: AtomicUsize::new(0),
            done: AtomicUsize::new(0),
        }
    }

    fn work(&self, expected: usize) {
        self.cpus.fetch_or(
            1u32 << Aarch64Cpu::current_id().as_index(),
            Ordering::Relaxed,
        );
        self.arrived.fetch_add(1, Ordering::Release);

        let mut spins = 0;
        while self.arrived.load(Ordering::Acquire) < expected && spins < BARRIER_SPINS {
            spin_loop();
            spins += 1;
        }

        self.done.fetch_add(1, Ordering::Release);
    }
}

fn run_workers() -> Option<Arc<Run>> {
    let workers = smp::online_cpus();
    let run = Arc::new(Run::new());
    let scheduler = syscall_bridge::scheduler();

    for _ in 0..workers {
        let run = run.clone();
        scheduler
            .spawn(SpawnConfig::new("smp-probe"), move || run.work(workers))
            .expect("worker thread spawns");
    }

    for _ in 0..WAIT_STEPS {
        if run.done.load(Ordering::Acquire) == workers {
            return Some(run);
        }
        scheduler.sleep_ms(WAIT_STEP_MS);
    }

    None
}

#[kernel_test]
fn threads_run_on_every_online_cpu() {
    let online = smp::online_cpus();
    let Some(run) = run_workers() else {
        panic!("worker threads did not finish in time");
    };

    let used = run.cpus.load(Ordering::Relaxed);

    assert_eq!(
        run.arrived.load(Ordering::Acquire),
        online,
        "every worker must reach the barrier"
    );
    assert_eq!(
        used.count_ones() as usize,
        online,
        "threads must reach every online CPU, mask {used:#x} of {online} CPU(s)"
    );
}
