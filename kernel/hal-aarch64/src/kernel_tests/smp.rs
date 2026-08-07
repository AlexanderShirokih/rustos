//! Проводка планировщика на нескольких CPU.
//!
//! Выбор CPU и учёт очередей покрыты host-тестами планировщика; здесь
//! проверяется то, что на host не воспроизводится: потоки действительно
//! исполняются на разных ядрах, а поднятый CPU обслуживает их сам.

use alloc::{sync::Arc, vec::Vec};
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
};

use capability::{ParkState, WaitToken};
use kernel_tests::kernel_test;
use kernelspace::syscall_bridge;
use scheduler::{ArchCpu, SchedulerServiceExt, SpawnConfig, with_preemption_disabled};

use crate::{sched::Aarch64Cpu, smp};

const BARRIER_SPINS: usize = 50_000_000;

const WAIT_STEP_MS: u64 = 1;
const WAIT_STEPS: usize = 2_000;

// Срок парковки работяги: перекос собирается за единицы миллисекунд, а
// провалившийся тест не оставляет потоков, ждущих пробуждения навсегда.
const PARK_TIMEOUT_NS: u64 = 5_000_000_000;

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
        self.cpus.fetch_or(current_cpu_bit(), Ordering::Relaxed);
        self.arrived.fetch_add(1, Ordering::Release);

        spin_while(|| self.arrived.load(Ordering::Acquire) < expected);

        self.done.fetch_add(1, Ordering::Release);
    }
}

// Перекос, собранный на одном CPU: работяги паркуются, пока соседние ядра
// заняты якорями, и просыпаются все разом в очередь своего CPU.
struct Backlog {
    started_on: AtomicU32,
    ran_on: AtomicU32,
    anchors_started: AtomicUsize,
    parked: AtomicUsize,
    done: AtomicUsize,
    park_states: Vec<AtomicU32>,
    anchors_released: AtomicBool,
    workers_released: AtomicBool,
}

impl Backlog {
    fn new(workers: usize) -> Self {
        Self {
            started_on: AtomicU32::new(0),
            ran_on: AtomicU32::new(0),
            anchors_started: AtomicUsize::new(0),
            parked: AtomicUsize::new(0),
            done: AtomicUsize::new(0),
            park_states: (0..workers)
                .map(|_| AtomicU32::new(ParkState::REGISTERED))
                .collect(),
            anchors_released: AtomicBool::new(false),
            workers_released: AtomicBool::new(false),
        }
    }

    fn anchor(&self) {
        self.anchors_started.fetch_add(1, Ordering::Release);
        spin_while(|| !self.anchors_released.load(Ordering::Acquire));
    }

    fn work(&self, index: usize) {
        self.started_on
            .fetch_or(current_cpu_bit(), Ordering::Relaxed);
        let state = &self.park_states[index];
        // Вытеснение между отметкой и парковкой оставило бы работягу в очереди
        // готовых, и следующий спавн ушёл бы к соседу: тест собирал бы перекос
        // на двух CPU.
        with_preemption_disabled::<Aarch64Cpu, _>(|| {
            state.store(ParkState::REGISTERED, Ordering::Release);
            self.parked.fetch_add(1, Ordering::Release);
            capability::runtime().block_current_until(state, Some(PARK_TIMEOUT_NS));
        });

        self.occupy_cpu();
        self.done.fetch_add(1, Ordering::Release);
    }

    // Маска пройденных ядер и есть след миграции.
    fn occupy_cpu(&self) {
        let mut marked = 0u32;
        spin_while(|| {
            let bit = current_cpu_bit();
            if bit != marked {
                marked = bit;
                self.ran_on.fetch_or(bit, Ordering::Relaxed);
            }
            !self.workers_released.load(Ordering::Acquire)
        });
    }

    fn wake(&self, workers: &[scheduler::ThreadId]) {
        for (index, thread) in workers.iter().enumerate() {
            self.park_states[index].store(ParkState::SIGNALED, Ordering::Release);
            capability::runtime().unblock(WaitToken::from(*thread));
        }
    }
}

fn current_cpu_bit() -> u32 {
    1u32 << Aarch64Cpu::current_id().as_index()
}

// Число оборотов ограничено: провалившийся тест не вешает машину на спине.
fn spin_while(mut hold: impl FnMut() -> bool) {
    let mut spins = 0;
    while hold() && spins < BARRIER_SPINS {
        spin_loop();
        spins += 1;
    }
}

fn wait_until(mut ready: impl FnMut() -> bool) -> bool {
    let scheduler = syscall_bridge::scheduler();
    for _ in 0..WAIT_STEPS {
        if ready() {
            return true;
        }
        scheduler.sleep_ms(WAIT_STEP_MS);
    }
    ready()
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

#[kernel_test]
fn a_backlog_on_one_cpu_is_drained_by_migration() {
    let online = smp::online_cpus();
    let anchors = online - 1;
    let worker_count = online + 1;
    let scheduler = syscall_bridge::scheduler();
    let backlog = Arc::new(Backlog::new(worker_count));

    // Занятые соседи выравнивают загрузку: новые потоки остаются на этом CPU.
    for _ in 0..anchors {
        let backlog = backlog.clone();
        scheduler
            .spawn(SpawnConfig::new("smp-anchor"), move || backlog.anchor())
            .expect("anchor thread spawns");
    }
    assert!(
        wait_until(|| backlog.anchors_started.load(Ordering::Acquire) == anchors),
        "anchors must occupy every other CPU"
    );

    // Работяга паркуется до спавна следующего: очередь этого CPU остаётся
    // пустой, и выбор CPU каждый раз повторяется.
    let mut workers: Vec<scheduler::ThreadId> = Vec::with_capacity(worker_count);
    for index in 0..worker_count {
        let task = backlog.clone();
        let thread = scheduler
            .spawn(SpawnConfig::new("smp-worker"), move || task.work(index))
            .expect("worker thread spawns");
        workers.push(thread);
        assert!(
            wait_until(|| backlog.parked.load(Ordering::Acquire) > index),
            "worker {index} must park before the next one is spawned"
        );
    }

    let built_on = backlog.started_on.load(Ordering::Relaxed);
    assert_eq!(
        built_on.count_ones(),
        1,
        "the backlog must be built on a single CPU, mask {built_on:#x}"
    );

    backlog.anchors_released.store(true, Ordering::Release);
    backlog.wake(&workers);

    let spread =
        wait_until(|| backlog.ran_on.load(Ordering::Acquire).count_ones() as usize >= online);
    backlog.workers_released.store(true, Ordering::Release);
    assert!(
        wait_until(|| backlog.done.load(Ordering::Acquire) == worker_count),
        "workers must finish"
    );

    let used = backlog.ran_on.load(Ordering::Relaxed);
    assert!(
        spread,
        "a backlog of {worker_count} threads must spread over every online CPU, mask {used:#x} of {online} CPU(s)"
    );
}

const AUDIT_THREADS: usize = 16;

const AUDIT_CYCLES: usize = 8;

const SETTLE_MATCHES: usize = 8;

fn free_frames() -> usize {
    crate::memory::memory_setup::frame_allocator().map_or(0, |allocator| allocator.free_frames())
}

fn load_cycle() {
    let scheduler = syscall_bridge::scheduler();
    let done = Arc::new(AtomicUsize::new(0));

    for _ in 0..AUDIT_THREADS {
        let done = done.clone();
        scheduler
            .spawn(SpawnConfig::new("audit"), move || {
                done.fetch_add(1, Ordering::Release);
            })
            .expect("audit thread spawns");
    }

    assert!(
        wait_until(|| done.load(Ordering::Acquire) == AUDIT_THREADS),
        "every audit thread must finish"
    );
}

fn settled_free_frames() -> usize {
    // Реап потока откладывается, пока его не отпустят все CPU, поэтому сразу
    // после круга остаток ещё меняется: замер берётся, когда он осел.
    let scheduler = syscall_bridge::scheduler();
    let mut last = free_frames();
    let mut matches = 0;
    for _ in 0..WAIT_STEPS {
        scheduler.sleep_ms(WAIT_STEP_MS);
        let free = free_frames();
        if free == last {
            matches += 1;
            if matches == SETTLE_MATCHES {
                return free;
            }
        } else {
            matches = 0;
            last = free;
        }
    }
    last
}

#[kernel_test]
fn repeated_thread_load_stops_spending_frames() {
    assert!(
        free_frames() > 0,
        "the frame allocator must be installed by this point"
    );

    // Куча ядра растёт под нагрузку и фреймы обратно не отдаёт, поэтому первые
    // круги расходуют их законно. Расход обязан прекратиться: круг, вернувший
    // всё, что взял, оставляет тот же остаток, что и предыдущий. Утечка на
    // круг не даёт такой пары никогда.
    let mut marks = Vec::with_capacity(AUDIT_CYCLES);
    for _ in 0..AUDIT_CYCLES {
        load_cycle();
        let free = settled_free_frames();
        if marks.last() == Some(&free) {
            return;
        }
        marks.push(free);
    }

    panic!("repeated load must stop spending frames, marks {marks:?}");
}
