//! Гонки между CPU за состояние планировщика: один std-поток моделирует один
//! CPU, а `MockCpu` держит его идентификатор и CPU-local указатель в
//! thread-local.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used, clippy::std_instead_of_core)]

mod common;

use std::{
    collections::HashSet,
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use scheduler::{
    CpuEntry, CpuId, Scheduler, SchedulerConfig, SchedulerHandle, SchedulerServiceExt, SpawnConfig,
    ThreadId, Uninit,
};

use crate::common::{
    MockContext, MockTimer, MockTimerSource, reset_switches, set_current_cpu, with_simulated_irq,
};

type TestScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;
type TestHandle = SchedulerHandle<MockContext, MockTimerSource>;

const CONFIG: SchedulerConfig = SchedulerConfig::new(32, 64);

const CPUS: u16 = 4;

const POOL: usize = 24;

const ROUNDS: usize = 256;

const SPAWN_EVERY: usize = 32;

const TICK_NS: u64 = scheduler::DEFAULT_QUANTUM_NS;

const DRAIN_TICKS: usize = POOL;

type RunningScheduler = Scheduler<MockContext, MockTimerSource, scheduler::Running>;

struct Contention {
    handle: TestHandle,
    timer: Arc<MockTimer>,
    now_ns: AtomicU64,
    start: Barrier,
    drain: Barrier,
    spawned: Mutex<Vec<ThreadId>>,
}

impl Contention {
    fn tick(&self) {
        let now_ns = self.now_ns.fetch_add(TICK_NS, Ordering::Relaxed) + TICK_NS;
        self.timer.advance_to(now_ns);
        with_simulated_irq(|| self.handle.on_timer_tick(now_ns));
    }

    fn reschedule(&self) {
        // Перевыбор по чужой просьбе идёт тем же путём, что обработчик SGI.
        with_simulated_irq(|| self.handle.on_reschedule());
    }

    fn spawn_worker(&self) {
        let thread = self
            .handle
            .spawn(SpawnConfig::new("worker"), || {})
            .expect("worker thread spawns");
        self.spawned.lock().expect("spawn list").push(thread);
    }

    fn drain(&self) {
        self.drain.wait();
        for _ in 0..DRAIN_TICKS {
            self.tick();
        }
    }
}

fn spawn_cpu(
    contention: &Arc<Contention>,
    raw: u16,
    work: impl Fn(&Contention, usize) + Send + 'static,
) -> thread::JoinHandle<()> {
    let contention = Arc::clone(contention);
    thread::spawn(move || {
        let id = CpuId::new(raw).expect("cpu id within MAX_CPUS");
        set_current_cpu(id);
        CpuEntry::bootstrap(&contention.handle).expect("bootstrap of a secondary cpu");
        contention.start.wait();
        for round in 0..ROUNDS {
            work(&contention, round);
        }
        contention.drain();
    })
}

fn make_contention(cpus: u16) -> (RunningScheduler, Arc<Contention>) {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let running = scheduler.run();
    let handle = running.handle();
    let contention = Arc::new(Contention {
        handle,
        timer,
        now_ns: AtomicU64::new(0),
        start: Barrier::new(usize::from(cpus)),
        drain: Barrier::new(usize::from(cpus)),
        spawned: Mutex::new(Vec::new()),
    });
    (running, contention)
}

fn fill_pool(running: &RunningScheduler, count: usize) -> Vec<ThreadId> {
    (0..count)
        .map(|index| {
            running
                .handle()
                .spawn(SpawnConfig::new("worker"), move || {
                    let _ = index;
                })
                .expect("worker thread spawns")
        })
        .collect()
}

#[test]
fn race_terminating_a_thread_while_cpus_tick_never_loses_its_slot() {
    let (running, contention) = make_contention(CPUS);
    let pool = fill_pool(&running, POOL);
    let objects: Vec<_> = pool
        .iter()
        .map(|id| running.thread_object_for(*id).expect("worker object"))
        .collect();

    let mut cpus: Vec<_> = (1..CPUS)
        .map(|raw| {
            spawn_cpu(&contention, raw, |contention, round| {
                if round % 2 == 0 {
                    contention.tick();
                } else {
                    contention.reschedule();
                }
            })
        })
        .collect();

    // Boot CPU терминирует пул, пока соседи вытесняют и переселяют его потоки.
    set_current_cpu(CpuId::ZERO);
    contention.start.wait();
    for (index, object) in objects.iter().enumerate() {
        contention
            .handle
            .terminate_thread(object, i32::try_from(index).expect("exit code fits"))
            .expect("terminating a live thread is accepted");
        contention.tick();
    }
    contention.drain();

    for cpu in cpus.drain(..) {
        cpu.join().expect("cpu thread joins");
    }

    for (index, object) in objects.iter().enumerate() {
        assert_eq!(
            object.exit_code(),
            i32::try_from(index).expect("exit code fits"),
            "a terminated thread keeps the exit code it was given"
        );
    }
    for id in &pool {
        assert!(
            running.thread_object_for(*id).is_none(),
            "thread {id:?} must leave the table once every cpu released it"
        );
    }

    // Слоты вернулись в оборот: пул заводится заново целиком.
    let refilled = fill_pool(&running, POOL);
    assert_eq!(refilled.len(), POOL);
}

#[test]
fn race_spawning_from_every_cpu_keeps_each_thread_on_one_cpu() {
    let (running, contention) = make_contention(CPUS);

    let mut cpus: Vec<_> = (1..CPUS)
        .map(|raw| {
            spawn_cpu(&contention, raw, |contention, round| {
                if round % SPAWN_EVERY == 0 {
                    contention.spawn_worker();
                }
                contention.tick();
            })
        })
        .collect();

    set_current_cpu(CpuId::ZERO);
    contention.start.wait();
    for round in 0..ROUNDS {
        if round % SPAWN_EVERY == 0 {
            contention.spawn_worker();
        }
        contention.tick();
    }
    contention.drain();

    for cpu in cpus.drain(..) {
        cpu.join().expect("cpu thread joins");
    }

    let pool = contention.spawned.lock().expect("spawn list");
    assert!(!pool.is_empty(), "every cpu must have spawned");

    // Тела потоков в моке не исполняются, завершиться им нечем: каждый
    // спавн обязан числиться в таблице и ровно за одним поднятым CPU.
    let mut seen = HashSet::with_capacity(pool.len());
    for id in pool.iter() {
        assert!(
            seen.insert(*id),
            "thread {id:?} was handed out to two spawns at once"
        );
        let cpu = running
            .thread_cpu_affinity(*id)
            .unwrap_or_else(|| panic!("thread {id:?} left the table without ever finishing"));
        assert!(
            cpu.raw() < CPUS,
            "thread {id:?} is homed to cpu {cpu:?} that was never brought up"
        );
    }
}
