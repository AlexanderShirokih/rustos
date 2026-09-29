//! Параллельный стресс syscall ABI: бригада потоков работает одновременно и
//! раскладывается ядром по всем CPU машины. Seed задаёт раскладку ролей, а
//! утверждения прогона - законы сохранения.

use alloc::{sync::Arc, vec::Vec};
use core::{
    fmt::Write as _,
    sync::atomic::{
        AtomicI64, AtomicU64,
        Ordering::{Acquire, Relaxed, Release},
    },
};

use kernel_objects::{Port, Resource, Signal, kind};
use kernel_tests::kernel_test;
use runtime::{
    JoinHandle, Priority, Timeout, UserMemFlags, handle_close, handle_duplicate, port_call,
    port_recv, port_reply, port_send, signal_wait_one, spawn_owned, thread_exit_code,
    thread_terminate, thread_yield,
};
use syscall::{Handle, Rights, SIGNALED, SYSCALL_RETURN_TIMEOUT, SyscallError};

use crate::stress::{SplitMix64, StressPlan, log_pass, log_seed, read_payload, write_payload};

const STACK_SIZE: u64 = 0x4000;

const CALLERS: usize = 3;
const RESPONDERS: usize = 3;
const MAPPERS: usize = 1;
const CHURNERS: usize = 1;

const BRIGADE: usize = CALLERS + RESPONDERS + MAPPERS + CHURNERS;

const RECV_TIMEOUT_MS: u64 = 500;
const POLL_MS: u64 = 100;

const RECV_TIMEOUT: Timeout = Timeout::from_millis(RECV_TIMEOUT_MS);
const POLL_TIMEOUT: Timeout = Timeout::from_millis(POLL_MS);
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(10);

const CALL_TIMEOUTS_MS: [u64; 4] = [0, 1, 5, 500];

const STALL_POLLS: usize = (8 * RECV_TIMEOUT_MS / POLL_MS) as usize;

const MAPPING_BYTES: u64 = 0x4000;

const VICTIMS: usize = 6;

const TERMINATE_ROUNDS: usize = 8;

const VICTIM_TIMEOUT: Timeout = Timeout::from_secs(10);

const VICTIM_EXIT_BASE: u32 = 100;

const READY_YIELD_LIMIT: usize = 100_000;

static VICTIMS_READY: AtomicU64 = AtomicU64::new(0);

struct Arena {
    server: Handle<kind::Port>,
    client: Handle<kind::Port>,
    callers_live: AtomicU64,
    calls_ok: AtomicU64,
    replies_ok: AtomicU64,
    replies_lost: AtomicU64,
    mismatched: AtomicU64,
    failures: AtomicU64,
    progress: AtomicU64,
    finished: AtomicU64,
    mappings: AtomicU64,
    duplicates: AtomicU64,
    first_failure: AtomicI64,
}

impl Arena {
    fn new(server: Handle<kind::Port>, client: Handle<kind::Port>) -> Self {
        Self {
            server,
            client,
            callers_live: AtomicU64::new(CALLERS as u64),
            calls_ok: AtomicU64::new(0),
            replies_ok: AtomicU64::new(0),
            replies_lost: AtomicU64::new(0),
            mismatched: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            progress: AtomicU64::new(0),
            finished: AtomicU64::new(0),
            mappings: AtomicU64::new(0),
            duplicates: AtomicU64::new(0),
            first_failure: AtomicI64::new(0),
        }
    }

    fn failed(&self, code: i64) {
        let _ = self
            .first_failure
            .compare_exchange(0, code, Relaxed, Relaxed);
        self.failures.fetch_add(1, Relaxed);
    }

    fn callers_running(&self) -> bool {
        self.callers_live.load(Acquire) > 0
    }

    fn step_done(&self) {
        self.progress.fetch_add(1, Relaxed);
    }

    fn worker_done(&self) {
        self.finished.fetch_add(1, Release);
    }
}

fn request_of(caller: usize, step: u64) -> u64 {
    // Номер вызывающего и номер шага лежат в одном значении. Ответ, пришедший
    // чужому потоку или перепутанный с другим шагом, виден по нему.
    ((caller as u64) << 56) | (step & 0x00FF_FFFF_FFFF_FFFF)
}

fn caller_of(request: u64) -> usize {
    (request >> 56) as usize
}

fn answer_to(request: u64) -> u64 {
    request.rotate_left(17)
}

fn failure_code(error: &kernel_objects::Error, fallback: i64) -> i64 {
    match error {
        kernel_objects::Error::Syscall(error) => -i64::from(*error as u32),
        _ => fallback,
    }
}

fn sleep(budget: Timeout) {
    // Ждём бит, который никто не поднимает, и выходим по сроку.
    let idle = Signal::create().expect("signal create for sleep");
    let _ = idle.wait(SIGNALED, budget);
}

fn caller(arena: &Arena, index: usize, steps: u64, seed: u64) -> u32 {
    let mut random = SplitMix64::new(seed);
    for step in 0..steps {
        // Срок выбирается на каждый вызов. Короткий уводит вызывающего раньше
        // ответа, и равенство итогов проверяется на этой гонке.
        let pick = (random.next() % CALL_TIMEOUTS_MS.len() as u64) as usize;
        let timeout = Timeout::from_millis(CALL_TIMEOUTS_MS[pick]);
        let request = request_of(index, step);
        if !write_payload(request) {
            arena.failed(-1);
            break;
        }
        let result = port_call(arena.client, timeout);
        if result == 0 {
            if read_payload() != Some(answer_to(request)) {
                arena.mismatched.fetch_add(1, Relaxed);
            }
            arena.calls_ok.fetch_add(1, Relaxed);
        } else if result != SYSCALL_RETURN_TIMEOUT {
            arena.failed(result);
            break;
        }
        arena.step_done();
    }
    arena.callers_live.fetch_sub(1, Release);
    arena.worker_done();
    0
}

fn responder(arena: &Arena) -> u32 {
    loop {
        let reply = match port_recv(arena.server, RECV_TIMEOUT) {
            Ok(reply) => reply,
            Err(SyscallError::Timeout) => {
                if !arena.callers_running() {
                    break;
                }
                continue;
            }
            Err(error) => {
                arena.failed(-i64::from(error as u32));
                break;
            }
        };
        let request = read_payload();
        match (request, reply) {
            (Some(request), Some(reply)) => {
                if caller_of(request) >= CALLERS {
                    arena.mismatched.fetch_add(1, Relaxed);
                }
                if !write_payload(answer_to(request)) {
                    arena.failed(-2);
                    handle_close(reply);
                } else if port_reply(reply) == 0 {
                    arena.replies_ok.fetch_add(1, Relaxed);
                } else {
                    // Вызывающий ушёл по сроку раньше ответа. Его вызов
                    // успешным не считался, и равенство итогов сохраняется.
                    arena.replies_lost.fetch_add(1, Relaxed);
                }
            }
            (_, reply) => {
                arena.mismatched.fetch_add(1, Relaxed);
                if let Some(reply) = reply {
                    handle_close(reply);
                }
            }
        }
        arena.step_done();
    }
    arena.worker_done();
    0
}

fn mapper(arena: &Arena) -> u32 {
    // Карта адресов у потоков процесса общая. Снятие идёт теми же вызовами,
    // что чужие установки на соседних CPU.
    while arena.callers_running() {
        let resource = match Resource::self_resource() {
            Ok(resource) => resource,
            Err(error) => {
                arena.failed(failure_code(&error, -10));
                break;
            }
        };
        let mapping = match resource.allocate(MAPPING_BYTES, UserMemFlags::ReadWrite) {
            Ok(mapping) => mapping,
            Err(error) => {
                arena.failed(failure_code(&error, -11));
                break;
            }
        };
        let churn: Vec<u64> = (0..64)
            .map(|value| value ^ arena.progress.load(Relaxed))
            .collect();
        if churn.len() != 64 || mapping.free().is_err() {
            arena.failures.fetch_add(1, Relaxed);
            break;
        }
        arena.mappings.fetch_add(1, Relaxed);
        arena.step_done();
    }
    arena.worker_done();
    0
}

fn handle_churn(arena: &Arena) -> u32 {
    // Закрытый слот переиспользуется под новый хэндл, и поколение отличает
    // его от прежнего.
    while arena.callers_running() {
        let derived =
            match handle_duplicate(arena.client, (Rights::WRITE | Rights::DUPLICATE).bits(), 0) {
                Ok(derived) => derived,
                Err(error) => {
                    arena.failed(-i64::from(error as u32));
                    break;
                }
            };
        let stale = derived.as_raw();
        let closed = handle_close(derived);
        if closed != 0 {
            arena.failed(closed);
            break;
        }
        // Закрытый хэндл больше не адресует объект. Слот мог достаться соседу,
        // и поколение отсекает обращение по устаревшему идентификатору.
        if port_send(stale, Timeout::POLL) == 0 {
            arena.mismatched.fetch_add(1, Relaxed);
        }
        arena.duplicates.fetch_add(1, Relaxed);
        arena.step_done();
    }
    arena.worker_done();
    0
}

fn await_brigade(arena: &Arena, workers: u64, seed: u64) {
    let mut last = arena.progress.load(Acquire);
    let mut idle_polls = 0;
    while arena.finished.load(Acquire) < workers {
        sleep(POLL_TIMEOUT);
        let progress = arena.progress.load(Acquire);
        if progress == last {
            idle_polls += 1;
        } else {
            idle_polls = 0;
            last = progress;
        }
        kernel_tests::kassert!(
            idle_polls < STALL_POLLS,
            "smp stress seed={seed} stalled: steps={progress} finished={} of {workers} \
             calls_ok={} replies_ok={} replies_lost={}",
            arena.finished.load(Acquire),
            arena.calls_ok.load(Relaxed),
            arena.replies_ok.load(Relaxed),
            arena.replies_lost.load(Relaxed)
        );
    }
}

fn run_brigade(seed: u64, steps: u64) {
    let port = Port::create().expect("stress port");
    // Клиентской стороне нужно и право множиться. Хэндлы создают от неё
    // производные, пока по ней идут вызовы.
    let client = port
        .duplicate(Rights::WRITE | Rights::DUPLICATE, None)
        .expect("client side of the stress port");
    let arena = Arc::new(Arena::new(port.as_raw(), client.as_raw()));

    let mut random = SplitMix64::new(seed);
    let mut workers: Vec<JoinHandle> = Vec::with_capacity(BRIGADE);
    for index in 0..CALLERS {
        let share = steps / 2 + random.next() % (steps / 2 + 1);
        let priority = Priority::new(1 + u8::try_from(random.next() % 2).unwrap_or(0));
        let caller_seed = random.next();
        let arena = Arc::clone(&arena);
        workers.push(
            spawn_owned(
                move || caller(&arena, index, share.max(1), caller_seed),
                STACK_SIZE,
                priority,
            )
            .expect("caller thread spawns"),
        );
    }
    for _ in 0..RESPONDERS {
        let arena = Arc::clone(&arena);
        workers.push(
            spawn_owned(move || responder(&arena), STACK_SIZE, Priority::new(1))
                .expect("responder thread spawns"),
        );
    }
    for _ in 0..MAPPERS {
        let arena = Arc::clone(&arena);
        workers.push(
            spawn_owned(move || mapper(&arena), STACK_SIZE, Priority::new(1))
                .expect("mapper thread spawns"),
        );
    }
    for _ in 0..CHURNERS {
        let arena = Arc::clone(&arena);
        workers.push(
            spawn_owned(move || handle_churn(&arena), STACK_SIZE, Priority::new(1))
                .expect("handle churn thread spawns"),
        );
    }

    await_brigade(&arena, BRIGADE as u64, seed);
    for worker in workers {
        worker
            .join(JOIN_TIMEOUT)
            .expect("a finished worker joins before timeout");
    }

    let calls_ok = arena.calls_ok.load(Relaxed);
    let replies_ok = arena.replies_ok.load(Relaxed);
    kernel_tests::kassert!(
        arena.failures.load(Relaxed) == 0,
        "smp stress seed={seed}: {} syscalls failed outside the model, first {}",
        arena.failures.load(Relaxed),
        arena.first_failure.load(Relaxed)
    );
    kernel_tests::kassert!(
        arena.mismatched.load(Relaxed) == 0,
        "smp stress seed={seed}: {} messages reached the wrong thread",
        arena.mismatched.load(Relaxed)
    );
    kernel_tests::kassert!(
        calls_ok == replies_ok,
        "smp stress seed={seed}: {calls_ok} completed calls against {replies_ok} delivered replies"
    );
    kernel_tests::kassert!(
        calls_ok > 0,
        "smp stress seed={seed}: no call completed, the run proved nothing"
    );
    report(seed, &arena);
}

fn report(seed: u64, arena: &Arena) {
    kernel_tests::runner::with_writer(|writer| {
        let mut output = kernel_tests::runner::FmtAdapter::new(writer);
        let _ = writeln!(
            output,
            "[SMP-STRESS: seed={seed} steps={} calls={} replies_lost={} maps={} handles={}]",
            arena.progress.load(Relaxed),
            arena.calls_ok.load(Relaxed),
            arena.replies_lost.load(Relaxed),
            arena.mappings.load(Relaxed),
            arena.duplicates.load(Relaxed)
        );
    });
}

fn shuffled(count: usize, random: &mut SplitMix64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..count).collect();
    for index in (1..count).rev() {
        let pick = (random.next() % (index as u64 + 1)) as usize;
        order.swap(index, pick);
    }
    order
}

fn terminate_round(random: &mut SplitMix64, seed: u64, round: usize) {
    // Жертвы блокируются в приёме на порту, которому никто не шлёт, и
    // снимаются прямо из syscall потоком, идущим на другом CPU.
    let port = Port::create().expect("victim port");
    let server = port.as_raw();

    VICTIMS_READY.store(0, Release);

    let mut victims: Vec<JoinHandle> = Vec::with_capacity(VICTIMS);
    for _ in 0..VICTIMS {
        victims.push(
            spawn_owned(
                move || {
                    // За отметкой готовности у жертвы нет обращений к общему
                    // состоянию процесса.
                    VICTIMS_READY.fetch_add(1, Release);
                    let result = port_recv(server, VICTIM_TIMEOUT);
                    // Возврата быть не должно. У порта нет отправителей, и приём
                    // обязан прерваться снятием.
                    u32::from(!matches!(result, Ok(None)))
                },
                STACK_SIZE,
                Priority::new(1),
            )
            .expect("victim thread spawns"),
        );
    }

    // Снятие потока не отпускает замки его процесса, а пролог потока держит
    // мьютекс кучи. Готовая жертва прошла пролог и кучу отпустила.
    let mut waits = 0;
    while VICTIMS_READY.load(Acquire) != VICTIMS as u64 {
        kernel_tests::kassert!(
            waits < READY_YIELD_LIMIT,
            "smp stress seed={seed} round={round}: only {} of {VICTIMS} victims ready",
            VICTIMS_READY.load(Acquire)
        );
        waits += 1;
        thread_yield();
    }

    for index in shuffled(VICTIMS, random) {
        let exit_code = VICTIM_EXIT_BASE + u32::try_from(index).unwrap_or(0);
        let result = thread_terminate(victims[index].handle(), u64::from(exit_code));
        kernel_tests::kassert!(
            result == 0,
            "smp stress seed={seed} round={round}: terminating victim {index} failed with {result}"
        );
    }

    for (index, victim) in victims.drain(..).enumerate() {
        let handle = victim.handle();
        let waited = signal_wait_one(handle, SIGNALED, JOIN_TIMEOUT);
        kernel_tests::kassert!(
            waited >= 0,
            "smp stress seed={seed} round={round}: victim {index} never terminated ({waited})"
        );
        // Код читается по живому хэндлу: `join` отдаёт стек и закрывает его,
        // а вместе с ним каскадом уходят и производные хэндлы.
        let expected = i64::from(VICTIM_EXIT_BASE + u32::try_from(index).unwrap_or(0));
        let code = thread_exit_code(handle);
        kernel_tests::kassert!(
            code == expected,
            "smp stress seed={seed} round={round}: victim {index} left code {code}, not {expected}"
        );
        victim
            .join(JOIN_TIMEOUT)
            .expect("a terminated victim joins before timeout");
    }
}

#[kernel_test(stress)]
fn smp_port_brigade_stress() {
    let plan = StressPlan::load();
    for seed in plan.seeds() {
        log_seed(seed, plan.steps);
        run_brigade(seed, plan.steps);
        log_pass(seed);
    }
}

#[kernel_test(stress)]
fn smp_terminate_blocked_threads_stress() {
    let plan = StressPlan::load();
    for seed in plan.seeds() {
        log_seed(seed, TERMINATE_ROUNDS as u64);
        let mut random = SplitMix64::new(seed);
        for round in 0..TERMINATE_ROUNDS {
            terminate_round(&mut random, seed, round);
        }
        log_pass(seed);
    }
}
