//! Параллельный стресс памяти. Потоки выделяют, размечают, сверяют и освобождают регионы,
//! затем доводят память машины до отказа выделения и под отказом проверяют создание
//! объектов ядра.

use alloc::{sync::Arc, vec::Vec};
use core::{
    fmt::Write as _,
    sync::atomic::{
        AtomicI64, AtomicU64, AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
    },
};

use kernel_objects::{Error, Mapping, MemoryRegion, Port, Resource, Signal};
use runtime::{MemoryAccess, Timeout, UserMemFlags, spawn_owned};
use syscall::{SIGNALED, SyscallError, WakeCount};
use userland::USERLAND_PAGE_SIZE;

use crate::stress::{SplitMix64, StressPlan, log_pass, log_seed, measure};

const CHURN_PAGES: [u64; 5] = [1, 4, 16, 64, 256];
const CHURN_LIVE: usize = 16;

const FILL_CHUNK_PAGES: u64 = 4096;
const FILL_HANDLES: usize = 512;

const WORKERS_MAX: usize = 12;

const WARM_UP_SEED: u64 = 0;
const WARM_UP_STEPS: u64 = 16;

const HEAP_RESERVE_BYTES: usize = 256 * 1024;
const RELEASE_RESERVE_PAGES: u64 = 256;

const POLL_TIMEOUT: Timeout = Timeout::from_millis(100);
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(10);

const STALL_POLLS: usize = 600;

const PAGE_WORDS: usize = (USERLAND_PAGE_SIZE as usize) / size_of::<u64>();

const PHASE_CHURN: usize = 0;
const PHASE_FILL: usize = 1;
const PHASE_RELEASE: usize = 2;

struct Block {
    region: MemoryRegion,
    pages: u64,
    tag: u64,
}

struct Shared {
    idle: Signal,
    phase: AtomicUsize,
    arrived: AtomicUsize,
    progress: AtomicU64,
    failures: AtomicU64,
    first_failure: AtomicI64,
    corrupted: AtomicU64,
    refusals: AtomicU64,
    held_pages: AtomicU64,
    exhausted: AtomicU64,
    capped: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
enum Probe {
    Granted,
    Refused,
}

impl Block {
    fn allocate(resource: &Resource, pages: u64, tag: u64) -> Result<(Self, u64), Error> {
        let region = resource.create_virtual(pages * USERLAND_PAGE_SIZE, MemoryAccess::RW)?;
        let view = Mapping::full(&region, UserMemFlags::ReadWrite)?;
        let dirty = mark_pages(view.va(), pages, tag);
        drop(view);
        Ok((Self { region, pages, tag }, dirty))
    }

    fn mismatches(&self) -> Result<u64, Error> {
        let view = Mapping::full(&self.region, UserMemFlags::ReadWrite)?;
        Ok(mismatched_pages(view.va(), self.pages, self.tag))
    }
}

impl Shared {
    fn new() -> Self {
        Self {
            idle: Signal::create().expect("memory stress idle signal"),
            phase: AtomicUsize::new(PHASE_CHURN),
            arrived: AtomicUsize::new(0),
            progress: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            first_failure: AtomicI64::new(0),
            corrupted: AtomicU64::new(0),
            refusals: AtomicU64::new(0),
            held_pages: AtomicU64::new(0),
            exhausted: AtomicU64::new(0),
            capped: AtomicU64::new(0),
        }
    }

    fn pause(&self, timeout: Timeout) {
        // Бит `idle` никто не поднимает, и ожидание кончается по сроку. Под исчерпанием
        // пауза не создаёт объектов.
        let _ = self.idle.wait(SIGNALED, timeout);
    }

    fn failed(&self, code: i64) {
        let _ = self
            .first_failure
            .compare_exchange(0, code, Relaxed, Relaxed);
        self.failures.fetch_add(1, Relaxed);
    }

    fn arrive_and_wait(&self, next: usize) {
        self.arrived.fetch_add(1, Release);
        while self.phase.load(Acquire) < next {
            self.pause(Timeout::from_millis(1));
        }
    }
}

impl Probe {
    const fn name(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Refused => "refused",
        }
    }
}

fn page_word(tag: u64, page: u64) -> u64 {
    // Слово ненулевое, поэтому страница с нулями разметке не соответствует.
    (tag ^ page.wrapping_mul(0x9E37_79B9_7F4A_7C15)) | 1
}

fn page_words(va: u64, page: u64) -> (*mut u64, *mut u64) {
    let base = (va + page * USERLAND_PAGE_SIZE) as *mut u64;
    // Слово в конце страницы лежит в той же странице, что и первое.
    (base, base.wrapping_add(PAGE_WORDS - 1))
}

fn mark_pages(va: u64, pages: u64, tag: u64) -> u64 {
    let mut dirty = 0;
    for page in 0..pages {
        let (first, last) = page_words(va, page);
        let word = page_word(tag, page);
        // SAFETY: ядро отобразило [va, va + pages * PAGE_SIZE) на запись этому процессу, оба
        // слова выровнены и лежат внутри страницы `page`; блоком владеет один поток.
        unsafe {
            if first.read_volatile() != 0 || last.read_volatile() != 0 {
                dirty += 1;
            }
            first.write_volatile(word);
            last.write_volatile(word);
        }
    }
    dirty
}

fn mismatched_pages(va: u64, pages: u64, tag: u64) -> u64 {
    let mut mismatched = 0;
    for page in 0..pages {
        let (first, last) = page_words(va, page);
        let word = page_word(tag, page);
        // SAFETY: см. mark_pages; отображение живо, пока жив блок или регион.
        let (head, tail) = unsafe { (first.read_volatile(), last.read_volatile()) };
        if head != word || tail != word {
            mismatched += 1;
        }
    }
    mismatched
}

fn refusal(error: Error) -> bool {
    matches!(
        error,
        Error::Syscall(SyscallError::OutOfMemory | SyscallError::ResourceExhausted)
    )
}

fn failure_code(error: Error, fallback: i64) -> i64 {
    match error {
        Error::Syscall(error) => -i64::from(error as u32),
        _ => fallback,
    }
}

fn verify(shared: &Shared, block: &Block) {
    match block.mismatches() {
        Ok(mismatched) => {
            shared.corrupted.fetch_add(mismatched, Relaxed);
        }
        Err(error) if refusal(error) => {
            shared.refusals.fetch_add(1, Relaxed);
        }
        Err(error) => shared.failed(failure_code(error, -20)),
    }
}

fn release(shared: &Shared, block: Block) {
    // Освобождение не выделяет кадров, поэтому отказ сверки здесь - расхождение, а не
    // нехватка.
    match block.mismatches() {
        Ok(mismatched) => {
            shared.corrupted.fetch_add(mismatched, Relaxed);
        }
        Err(error) => shared.failed(failure_code(error, -21)),
    }
    drop(block);
}

fn anonymous(shared: &Shared, resource: &Resource, pages: u64, tag: u64) {
    match resource.allocate(pages * USERLAND_PAGE_SIZE, UserMemFlags::ReadWrite) {
        Ok(mapping) => {
            let dirty = mark_pages(mapping.va(), pages, tag);
            let mismatched = mismatched_pages(mapping.va(), pages, tag);
            shared.corrupted.fetch_add(dirty + mismatched, Relaxed);
            if let Err(error) = mapping.free() {
                shared.failed(failure_code(error, -22));
            }
        }
        Err(error) if refusal(error) => {
            shared.refusals.fetch_add(1, Relaxed);
        }
        Err(error) => shared.failed(failure_code(error, -23)),
    }
}

fn aliased(shared: &Shared, resource: &Resource, pages: u64, tag: u64) {
    // Два отображения одного региона указывают на одни кадры, и разметка через первое
    // читается через второе.
    let region = match resource.create_virtual(pages * USERLAND_PAGE_SIZE, MemoryAccess::RW) {
        Ok(region) => region,
        Err(error) if refusal(error) => {
            shared.refusals.fetch_add(1, Relaxed);
            return;
        }
        Err(error) => return shared.failed(failure_code(error, -30)),
    };
    let views = Mapping::full(&region, UserMemFlags::ReadWrite)
        .map_err(Error::from)
        .and_then(|writer| Ok((Mapping::full(&region, UserMemFlags::ReadWrite)?, writer)));
    match views {
        Ok((reader, writer)) => {
            mark_pages(writer.va(), pages, tag);
            let mismatched = mismatched_pages(reader.va(), pages, tag);
            shared.corrupted.fetch_add(mismatched, Relaxed);
        }
        Err(error) if refusal(error) => {
            shared.refusals.fetch_add(1, Relaxed);
        }
        Err(error) => shared.failed(failure_code(error, -31)),
    }
}

fn churn(shared: &Shared, resource: &Resource, random: &mut SplitMix64, steps: u64) {
    let mut live: Vec<Block> = Vec::with_capacity(CHURN_LIVE);
    for _ in 0..steps {
        let action = random.next() % 8;
        let pages = CHURN_PAGES[(random.next() % CHURN_PAGES.len() as u64) as usize];
        let tag = random.next();
        let pick = random.next() as usize;
        match action {
            0..=2 => {
                if live.len() == CHURN_LIVE {
                    release(shared, live.swap_remove(pick % live.len()));
                }
                match Block::allocate(resource, pages, tag) {
                    Ok((block, dirty)) => {
                        shared.corrupted.fetch_add(dirty, Relaxed);
                        live.push(block);
                    }
                    Err(error) if refusal(error) => {
                        shared.refusals.fetch_add(1, Relaxed);
                        if !live.is_empty() {
                            release(shared, live.swap_remove(pick % live.len()));
                        }
                    }
                    Err(error) => shared.failed(failure_code(error, -10)),
                }
            }
            3 => aliased(shared, resource, pages, tag),
            4 => anonymous(shared, resource, pages, tag),
            5 if !live.is_empty() => verify(shared, &live[pick % live.len()]),
            _ if !live.is_empty() => release(shared, live.swap_remove(pick % live.len())),
            _ => {}
        }
        shared.progress.fetch_add(1, Relaxed);
    }
    for block in live {
        release(shared, block);
    }
}

fn fill(
    shared: &Shared,
    resource: &Resource,
    random: &mut SplitMix64,
    held: &mut Vec<Block>,
    limit: usize,
) {
    // После каждого отказа кусок уменьшается вдвое, пока не откажет выделение одной
    // страницы. Тогда нехватку получают и выделения самого ядра.
    let mut pages = FILL_CHUNK_PAGES;
    loop {
        if held.len() == limit {
            shared.capped.fetch_add(1, Relaxed);
            return;
        }
        match Block::allocate(resource, pages, random.next()) {
            Ok((block, dirty)) => {
                shared.corrupted.fetch_add(dirty, Relaxed);
                shared.held_pages.fetch_add(pages, Relaxed);
                held.push(block);
            }
            Err(error) if refusal(error) && pages == 1 => {
                shared.exhausted.fetch_add(1, Relaxed);
                return;
            }
            Err(error) if refusal(error) => pages /= 2,
            Err(error) => return shared.failed(failure_code(error, -40)),
        }
        shared.progress.fetch_add(1, Relaxed);
    }
}

fn worker(shared: &Shared, seed: u64, steps: u64, threads: usize) -> u32 {
    let mut random = SplitMix64::new(seed);
    // Ёмкость берётся до нагрузки, чтобы под исчерпанием куча не росла.
    let limit = FILL_HANDLES / threads;
    let mut held: Vec<Block> = Vec::with_capacity(limit);
    match Resource::self_resource() {
        Ok(resource) => {
            churn(shared, &resource, &mut random, steps);
            shared.arrive_and_wait(PHASE_FILL);
            fill(shared, &resource, &mut random, &mut held, limit);
        }
        Err(error) => {
            shared.failed(failure_code(error, -1));
            shared.arrive_and_wait(PHASE_FILL);
        }
    }
    shared.arrive_and_wait(PHASE_RELEASE);
    for block in held {
        release(shared, block);
        shared.progress.fetch_add(1, Relaxed);
    }
    shared.arrived.fetch_add(1, Release);
    0
}

fn await_arrivals(shared: &Shared, workers: usize, seed: u64, phase: &str) {
    let mut last = shared.progress.load(Relaxed);
    let mut idle_polls = 0;
    while shared.arrived.load(Acquire) < workers {
        shared.pause(POLL_TIMEOUT);
        let progress = shared.progress.load(Relaxed);
        if progress == last {
            idle_polls += 1;
        } else {
            idle_polls = 0;
            last = progress;
        }
        test_harness::check!(
            idle_polls < STALL_POLLS,
            "memory stress seed={seed} stalled in {phase}: {} of {workers} workers arrived",
            shared.arrived.load(Acquire)
        );
    }
    shared.arrived.store(0, Relaxed);
}

fn probe_port() -> Result<Probe, Error> {
    match Port::create() {
        Ok(port) => match port.recv(Timeout::POLL) {
            Err(Error::Syscall(SyscallError::Timeout)) => Ok(Probe::Granted),
            Err(error) => Err(error),
            Ok(_) => Err(Error::Syscall(SyscallError::InvalidArgument)),
        },
        Err(error) if refusal(error) => Ok(Probe::Refused),
        Err(error) => Err(error),
    }
}

fn probe_signal() -> Result<Probe, Error> {
    match Signal::create() {
        Ok(signal) => {
            signal.set(SIGNALED, 0, WakeCount::All)?;
            let observed = signal.wait(SIGNALED, Timeout::POLL)?;
            if observed == SIGNALED {
                Ok(Probe::Granted)
            } else {
                Err(Error::Syscall(SyscallError::InvalidArgument))
            }
        }
        Err(error) if refusal(error) => Ok(Probe::Refused),
        Err(error) => Err(error),
    }
}

fn probe_thread() -> Result<Probe, Error> {
    match spawn_owned(|| 0) {
        Ok(thread) => {
            thread.join(JOIN_TIMEOUT).map_err(Error::Syscall)?;
            Ok(Probe::Granted)
        }
        Err(SyscallError::OutOfMemory | SyscallError::ResourceExhausted) => Ok(Probe::Refused),
        Err(error) => Err(Error::Syscall(error)),
    }
}

fn run_seed(seed: u64, steps: u64, threads: usize) {
    let shared = Arc::new(Shared::new());
    let mut random = SplitMix64::new(seed);
    let mut workers = Vec::with_capacity(threads);
    for _ in 0..threads {
        let shared = Arc::clone(&shared);
        let worker_seed = random.next();
        workers.push(
            spawn_owned(move || worker(&shared, worker_seed, steps, threads))
                .expect("memory stress worker spawns"),
        );
    }
    // Запас кучи берётся и отдаётся до исчерпания, чтобы проба потока под нехваткой
    // разместила своё замыкание в уже выданных куче страницах.
    drop(Vec::<u8>::with_capacity(HEAP_RESERVE_BYTES));
    // Резерв отдаётся после проб. Сверка при освобождении отображает блоки, и таблицы
    // страниц получают кадры из него.
    let reserve = Resource::self_resource()
        .and_then(|resource| {
            resource.create_virtual(RELEASE_RESERVE_PAGES * USERLAND_PAGE_SIZE, MemoryAccess::RW)
        })
        .expect("memory stress release reserve");

    await_arrivals(&shared, threads, seed, "churn");
    shared.phase.store(PHASE_FILL, Release);
    await_arrivals(&shared, threads, seed, "fill");

    let probes = [
        ("port", probe_port()),
        ("signal", probe_signal()),
        ("thread", probe_thread()),
    ];

    drop(reserve);
    shared.phase.store(PHASE_RELEASE, Release);
    await_arrivals(&shared, threads, seed, "release");
    for worker in workers {
        worker
            .join(JOIN_TIMEOUT)
            .expect("a memory stress worker joins before timeout");
    }

    for (name, probe) in probes {
        test_harness::check!(
            probe.is_ok(),
            "memory stress seed={seed}: {name} under exhaustion failed outside the model: {:?}",
            probe.err()
        );
    }
    test_harness::check!(
        shared.failures.load(Relaxed) == 0,
        "memory stress seed={seed}: {} memory calls failed outside the model, first {}",
        shared.failures.load(Relaxed),
        shared.first_failure.load(Relaxed)
    );
    // Доля памяти работника зависит от планирования. Работник, дошедший до предела
    // хэндлов, уступает память остальным, и исчерпание подтверждает отказ у любого из них.
    test_harness::check!(
        shared.exhausted.load(Relaxed) > 0,
        "memory stress seed={seed}: no worker reached allocation refusal, {} hit the handle limit",
        shared.capped.load(Relaxed)
    );
    test_harness::check!(
        shared.corrupted.load(Relaxed) == 0,
        "memory stress seed={seed}: {} pages lost their marking",
        shared.corrupted.load(Relaxed)
    );
    report(seed, &shared, &probes);
}

fn report(seed: u64, shared: &Shared, probes: &[(&str, Result<Probe, Error>); 3]) {
    test_harness::runner::with_writer(|writer| {
        let mut output = test_harness::runner::FmtAdapter::new(writer);
        let _ = write!(
            output,
            "[MEMORY-STRESS: seed={seed} steps={} refusals={} held_pages={} capped={}",
            shared.progress.load(Relaxed),
            shared.refusals.load(Relaxed),
            shared.held_pages.load(Relaxed),
            shared.capped.load(Relaxed)
        );
        for (name, probe) in probes {
            if let Ok(probe) = probe {
                let _ = write!(output, " {name}={}", probe.name());
            }
        }
        let _ = writeln!(output, "]");
    });
}

#[test_harness::test(stress)]
fn smp_memory_pressure_stress() {
    let plan = StressPlan::load();
    // Работник занимает в карте процесса стек и IPC-буфер, а сверка псевдонима
    // добавляет ещё два вида. Карта процесса вмещает 64 отображения, и при двенадцати
    // работниках в ней остаётся место главному потоку, куче и пробе потока.
    let workers = plan.threads.min(WORKERS_MAX);

    // Куча процесса растёт под ёмкости потоков и не отдаёт страниц, поэтому базовая
    // величина снимается после прогревочного seed.
    run_seed(WARM_UP_SEED, WARM_UP_STEPS, workers);
    let base = measure(format_args!("before the first memory seed"));

    for seed in plan.seeds() {
        log_seed(seed, plan.steps);
        run_seed(seed, plan.steps, workers);
        let budget = measure(format_args!("memory seed={seed}"));
        test_harness::check!(
            budget == base,
            "memory stress seed={seed}: budget base={base} now={budget}"
        );
        log_pass(seed);
    }
}
