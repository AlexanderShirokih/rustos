//! Воспроизводимый userspace stress-прогон Port, Signal и lifecycle через syscall ABI.

use alloc::{sync::Arc, vec::Vec};
use core::{
    fmt::{self, Write as _},
    marker::PhantomData,
    num::{NonZeroU32, NonZeroU64},
    sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::SeqCst},
};

use kernel_objects::{Error, ObjectKind, Port, Resource, Signal, kind};
use kernel_tests::kernel_test;
use rootkeeper::SystemImage;
use runtime::{
    JoinHandle, Priority, Timeout, handle_close, ipc_buffer_ptr, port_call, port_send,
    signal_create, signal_wait_one, spawn,
};
use syscall::{Handle, Rights, SIGNALED, SyscallError, WakeCount, decode_tag, encode_tag};

const PLAN_PATH: &str = "/testrunner/stress-plan";

const STACK_SIZE: u64 = 0x4000;
const OPERATION_TIMEOUT: Timeout = Timeout::from_secs(1);
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(2);
const READY_SPIN_LIMIT: usize = 10_000_000;

const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
const MIX_MULTIPLIER_HIGH: u64 = 0xBF58_476D_1CE4_E5B9;
const MIX_MULTIPLIER_LOW: u64 = 0x94D0_49BB_1331_11EB;

const POOL_CAPACITY: usize = 8;

const STANDALONE_OPERATIONS: u64 = 3;

pub(crate) struct SplitMix64 {
    state: u64,
}

pub(crate) struct StressPlan {
    first_seed: u64,
    seed_count: u64,
    pub(crate) steps: u64,
    epoch_steps: u64,
}

impl SplitMix64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GOLDEN_GAMMA);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(MIX_MULTIPLIER_HIGH);
        value = (value ^ (value >> 27)).wrapping_mul(MIX_MULTIPLIER_LOW);
        value ^ (value >> 31)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanError<'a> {
    Malformed(&'a str),
    Unknown(&'a str),
    Repeated(&'a str),
    NotANumber(&'a str),
    Missing(&'a str),
    Zero(&'a str),
}

impl<'a> PlanError<'a> {
    pub(crate) fn field(self) -> &'a str {
        match self {
            Self::Malformed(field)
            | Self::Unknown(field)
            | Self::Repeated(field)
            | Self::NotANumber(field)
            | Self::Missing(field)
            | Self::Zero(field) => field,
        }
    }

    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::Malformed(_) => "line is not a key=value pair",
            Self::Unknown(_) => "unknown key",
            Self::Repeated(_) => "key is declared twice",
            Self::NotANumber(_) => "value is not a number",
            Self::Missing(_) => "key is not declared",
            Self::Zero(_) => "key is declared zero",
        }
    }
}

impl StressPlan {
    pub(crate) fn parse(text: &str) -> Result<Self, PlanError<'_>> {
        let mut first_seed = None;
        let mut seed_count = None;
        let mut steps = None;
        let mut epoch_steps = None;

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(PlanError::Malformed(line));
            };
            let slot = match key {
                "seed" => &mut first_seed,
                "seeds" => &mut seed_count,
                "steps" => &mut steps,
                "epoch" => &mut epoch_steps,
                _ => return Err(PlanError::Unknown(key)),
            };
            if slot.is_some() {
                return Err(PlanError::Repeated(key));
            }
            let Ok(number) = value.parse::<u64>() else {
                return Err(PlanError::NotANumber(key));
            };
            *slot = Some(number);
        }

        let first_seed = first_seed.ok_or(PlanError::Missing("seed"))?;
        let seed_count = seed_count.ok_or(PlanError::Missing("seeds"))?;
        let steps = steps.ok_or(PlanError::Missing("steps"))?;
        let epoch_steps = epoch_steps.ok_or(PlanError::Missing("epoch"))?;
        if seed_count == 0 {
            return Err(PlanError::Zero("seeds"));
        }
        if steps == 0 {
            return Err(PlanError::Zero("steps"));
        }
        if epoch_steps == 0 {
            return Err(PlanError::Zero("epoch"));
        }

        Ok(Self {
            first_seed,
            seed_count,
            steps,
            epoch_steps,
        })
    }

    pub(crate) fn load() -> Self {
        let image =
            SystemImage::acquire(crate::bootstrap_handle()).expect("userland image is mapped");
        let entries = image.entries().expect("userland image decodes");
        let record = entries.entry(PLAN_PATH);
        kernel_tests::kassert!(record.is_some(), "stress plan is missing at {PLAN_PATH}");
        let record = record.expect("the record is present");

        let text = core::str::from_utf8(record.data());
        kernel_tests::kassert!(text.is_ok(), "stress plan at {PLAN_PATH} is not UTF-8");
        let text = text.expect("the record is UTF-8");

        let parsed = Self::parse(text);
        if let Err(error) = parsed {
            kernel_tests::kassert!(
                false,
                "stress plan at {PLAN_PATH}: {} '{}'",
                error.describe(),
                error.field()
            );
        }
        parsed.expect("the plan parses")
    }

    pub(crate) fn seeds(&self) -> impl Iterator<Item = u64> {
        let first_seed = self.first_seed;
        (0..self.seed_count).map(move |offset| first_seed.wrapping_add(offset))
    }

    pub(crate) const fn epochs(&self) -> u64 {
        self.steps.div_ceil(self.epoch_steps)
    }

    pub(crate) const fn epoch_length(&self, epoch: u64) -> u64 {
        let done = epoch * self.epoch_steps;
        let left = self.steps.saturating_sub(done);
        if left < self.epoch_steps {
            left
        } else {
            self.epoch_steps
        }
    }
}

pub(crate) const fn epoch_seed(seed: u64, epoch: u64) -> u64 {
    // Нулевая эпоха идёт по seed прогона. Падение дальней эпохи повторяется
    // прогоном одной эпохи по напечатанному значению.
    seed ^ epoch.wrapping_mul(GOLDEN_GAMMA)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    Expired {
        phase: &'static str,
        timeout: Timeout,
    },
    NotStarted {
        spins: usize,
    },
    Violated {
        result: i64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StepFailure {
    operation: &'static str,
    kind: FailureKind,
}

impl FailureKind {
    fn from_return(phase: &'static str, timeout: Timeout, result: i64) -> Self {
        if SyscallError::from_return(result) == Some(SyscallError::Timeout) {
            Self::Expired { phase, timeout }
        } else {
            Self::Violated { result }
        }
    }

    fn from_error(phase: &'static str, timeout: Timeout, error: Error, result: i64) -> Self {
        if error == Error::Syscall(SyscallError::Timeout) {
            Self::Expired { phase, timeout }
        } else {
            Self::Violated { result }
        }
    }

    const fn violated(result: i64) -> Self {
        Self::Violated { result }
    }

    pub(crate) const fn expired(self) -> bool {
        matches!(self, Self::Expired { .. } | Self::NotStarted { .. })
    }
}

impl fmt::Display for FailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expired { phase, timeout } => write!(
                f,
                "outcome=expired phase={phase} timeout_ns={}",
                timeout.raw()
            ),
            Self::NotStarted { spins } => write!(f, "outcome=expired phase=start spins={spins}"),
            Self::Violated { result } => write!(f, "outcome=violated result={result}"),
        }
    }
}

impl StepFailure {
    fn of(operation: &'static str, kind: FailureKind) -> Self {
        Self { operation, kind }
    }

    pub(crate) const fn expired(self) -> bool {
        self.kind.expired()
    }
}

impl fmt::Display for StepFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "operation={} {}", self.operation, self.kind)
    }
}

struct WorkerState<K: ObjectKind> {
    handle: u32,
    payload: u64,
    ready: AtomicBool,
    result: AtomicI64,
    observed: AtomicU64,
    kind: PhantomData<K>,
}

impl<K: ObjectKind> WorkerState<K> {
    fn new(handle: Handle<K>, payload: u64) -> Arc<Self> {
        Arc::new(Self {
            handle: handle.raw(),
            payload,
            ready: AtomicBool::new(false),
            result: AtomicI64::new(i64::MIN),
            observed: AtomicU64::new(0),
            kind: PhantomData,
        })
    }

    fn handle(&self) -> Option<Handle<K>> {
        NonZeroU32::new(self.handle).map(Handle::from_abi)
    }
}

fn worker_state<K: ObjectKind>(arg: usize) -> Arc<WorkerState<K>> {
    // SAFETY: arg получен из Arc::into_raw в spawn_worker ровно для этого потока;
    // from_raw забирает ту самую ссылку и освобождает её на выходе из worker.
    unsafe { Arc::from_raw(arg as *const WorkerState<K>) }
}

fn spawn_worker<K: ObjectKind>(
    entry: extern "C" fn(usize) -> u32,
    state: &Arc<WorkerState<K>>,
    spawn_error: i64,
) -> Result<JoinHandle, FailureKind> {
    let arg = Arc::into_raw(Arc::clone(state)) as usize;
    spawn(entry, arg, STACK_SIZE, Priority::new(1)).map_err(|_| {
        // SAFETY: поток не стартовал, ссылку из into_raw забрать больше некому.
        drop(unsafe { Arc::from_raw(arg as *const WorkerState<K>) });
        FailureKind::violated(spawn_error)
    })
}

pub(crate) fn write_payload(payload: u64) -> bool {
    let Some(buffer) = ipc_buffer_ptr() else {
        return false;
    };
    // SAFETY: текущий поток единолично владеет своим IPC-буфером; tag и первые
    // восемь байт data лежат внутри одной выделенной ядром страницы.
    unsafe {
        let buffer = &mut *buffer;
        buffer.tag = encode_tag(size_of::<u64>(), 0);
        buffer.data[..size_of::<u64>()].copy_from_slice(&payload.to_le_bytes());
    }
    true
}

pub(crate) fn read_payload() -> Option<u64> {
    let buffer = ipc_buffer_ptr()?;
    // SAFETY: текущий поток единолично читает свой IPC-буфер после завершения
    // rendezvous; границы data подтверждаются декодированным tag.
    unsafe {
        let buffer = &*buffer;
        let decoded = decode_tag(buffer.tag);
        if decoded.len != size_of::<u64>() {
            return None;
        }
        Some(u64::from_le_bytes(
            buffer.data[..size_of::<u64>()].try_into().ok()?,
        ))
    }
}

extern "C" fn send_worker(arg: usize) -> u32 {
    let state = worker_state::<kind::Port>(arg);
    state.ready.store(true, SeqCst);
    if !write_payload(state.payload) {
        state.result.store(-1000, SeqCst);
        return 0;
    }
    let result = state
        .handle()
        .map_or(-1001, |handle| port_send(handle, OPERATION_TIMEOUT));
    state.result.store(result, SeqCst);
    0
}

extern "C" fn call_worker(arg: usize) -> u32 {
    let state = worker_state::<kind::Port>(arg);
    state.ready.store(true, SeqCst);
    if !write_payload(state.payload) {
        state.result.store(-1000, SeqCst);
        return 0;
    }
    let result = state
        .handle()
        .map_or(-1001, |handle| port_call(handle, OPERATION_TIMEOUT));
    if result == 0 {
        state.observed.store(read_payload().unwrap_or(0), SeqCst);
    }
    state.result.store(result, SeqCst);
    0
}

extern "C" fn signal_wait_worker(arg: usize) -> u32 {
    let state = worker_state::<kind::Signal>(arg);
    state.ready.store(true, SeqCst);
    let result = state.handle().map_or(-1001, |handle| {
        signal_wait_one(handle, SIGNALED, OPERATION_TIMEOUT)
    });
    state.result.store(result, SeqCst);
    0
}

fn wait_until_ready<K: ObjectKind>(state: &WorkerState<K>) -> Result<(), FailureKind> {
    for _ in 0..READY_SPIN_LIMIT {
        if state.ready.load(SeqCst) {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(FailureKind::NotStarted {
        spins: READY_SPIN_LIMIT,
    })
}

fn join(worker: JoinHandle) -> Result<(), FailureKind> {
    worker
        .join(JOIN_TIMEOUT)
        .map_err(|error| FailureKind::from_error("join", JOIN_TIMEOUT, error.into(), -1002))
}

fn worker_outcome<K: ObjectKind>(state: &WorkerState<K>) -> Result<(), FailureKind> {
    match state.result.load(SeqCst) {
        0 => Ok(()),
        result => Err(FailureKind::from_return(
            "worker",
            OPERATION_TIMEOUT,
            result,
        )),
    }
}

fn port_send_recv(
    port: &Port,
    sender: Handle<kind::Port>,
    payload: u64,
) -> Result<(), FailureKind> {
    let state = WorkerState::new(sender, payload);
    let worker = spawn_worker(send_worker, &state, -1101)?;
    wait_until_ready(&state)?;

    let mut bytes = [0u8; size_of::<u64>()];
    let (len, reply) = port
        .recv_bytes(&mut bytes, OPERATION_TIMEOUT)
        .map_err(|error| FailureKind::from_error("recv", OPERATION_TIMEOUT, error, -1102))?;
    if len != bytes.len() || reply.is_some() || u64::from_le_bytes(bytes) != payload {
        return Err(FailureKind::violated(-1103));
    }
    join(worker)?;
    worker_outcome(&state)
}

fn port_call_reply(
    port: &Port,
    sender: Handle<kind::Port>,
    payload: u64,
) -> Result<(), FailureKind> {
    let state = WorkerState::new(sender, payload);
    let worker = spawn_worker(call_worker, &state, -1201)?;
    wait_until_ready(&state)?;

    let mut bytes = [0u8; size_of::<u64>()];
    let (len, reply) = port
        .recv_bytes(&mut bytes, OPERATION_TIMEOUT)
        .map_err(|error| FailureKind::from_error("recv", OPERATION_TIMEOUT, error, -1202))?;
    let Some(reply) = reply else {
        return Err(FailureKind::violated(-1203));
    };
    if len != bytes.len() || u64::from_le_bytes(bytes) != payload {
        return Err(FailureKind::violated(-1204));
    }
    let response = payload.rotate_left(17);
    reply
        .reply_bytes(&response.to_le_bytes())
        .map_err(|error| FailureKind::from_error("reply", OPERATION_TIMEOUT, error, -1205))?;
    join(worker)?;
    worker_outcome(&state)?;
    if state.observed.load(SeqCst) == response {
        Ok(())
    } else {
        Err(FailureKind::violated(-1206))
    }
}

fn signal_set_wait(
    signal: &Signal,
    waiter: Handle<kind::Signal>,
    payload: u64,
) -> Result<(), FailureKind> {
    let state = WorkerState::new(waiter, payload);
    let worker = spawn_worker(signal_wait_worker, &state, -1301)?;
    wait_until_ready(&state)?;

    signal
        .set(SIGNALED, 0, WakeCount::All)
        .map_err(|error| FailureKind::from_error("set", OPERATION_TIMEOUT, error, -1302))?;
    join(worker)?;
    // Сигнал сохраняется между шагами в пуле, и снятый бит возвращает следующему ждущему
    // блокировку до `set`: порядок пробуждения остаётся под проверкой.
    signal
        .set(0, SIGNALED, WakeCount::All)
        .map_err(|error| FailureKind::from_error("clear", OPERATION_TIMEOUT, error, -1303))?;
    let observed = state.result.load(SeqCst);
    if observed == i64::from(SIGNALED) {
        Ok(())
    } else {
        Err(FailureKind::from_return(
            "worker",
            OPERATION_TIMEOUT,
            observed,
        ))
    }
}

fn signal_close_wait(payload: u64) -> Result<(), FailureKind> {
    let signal = signal_create().map_err(|_| FailureKind::violated(-1400))?;
    let state = WorkerState::new(signal.as_raw(), payload);
    let worker = spawn_worker(signal_wait_worker, &state, -1401)?;
    wait_until_ready(&state)?;

    let close_result = handle_close(signal);
    if close_result != 0 {
        return Err(FailureKind::violated(close_result));
    }
    join(worker)?;
    let result = state.result.load(SeqCst);
    match SyscallError::from_return(result) {
        Some(SyscallError::Canceled | SyscallError::BadHandle) => Ok(()),
        _ => Err(FailureKind::from_return(
            "worker",
            OPERATION_TIMEOUT,
            result,
        )),
    }
}

fn port_peer_close(payload: u64) -> Result<(), FailureKind> {
    let port = Port::create().map_err(|_| FailureKind::violated(-1500))?;
    let client = port
        .duplicate(Rights::WRITE, Some(NonZeroU64::MIN.saturating_add(payload)))
        .map_err(|_| FailureKind::violated(-1501))?;
    let state = WorkerState::new(client.as_raw(), payload);
    let worker = spawn_worker(send_worker, &state, -1502)?;
    wait_until_ready(&state)?;

    drop(port);
    join(worker)?;
    let result = state.result.load(SeqCst);
    match SyscallError::from_return(result) {
        Some(SyscallError::PeerClosed | SyscallError::Revoked) => Ok(()),
        _ => Err(FailureKind::from_return(
            "worker",
            OPERATION_TIMEOUT,
            result,
        )),
    }
}

fn port_poll_timeout() -> Result<(), FailureKind> {
    let port = Port::create().map_err(|_| FailureKind::violated(-1600))?;
    match port.recv(Timeout::POLL) {
        Err(Error::Syscall(SyscallError::Timeout)) => Ok(()),
        _ => Err(FailureKind::violated(-1601)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepKind {
    Create,
    Use,
    Transfer,
    Release,
    Standalone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Step {
    kind: StepKind,
    index: usize,
    variant: u64,
    payload: u64,
}

pub(crate) fn choose(random: &mut SplitMix64, pool_len: usize) -> Step {
    // Вид шага, элемент пула и вариант операции приходят из одной
    // последовательности. Прогон повторяется по seed целиком.
    let payload = random.next();
    let selector = random.next();
    let variant = random.next();
    let position = random.next();

    let kind = if pool_len == 0 {
        StepKind::Create
    } else {
        match selector % 8 {
            0 | 1 => StepKind::Create,
            2..=4 => StepKind::Use,
            5 => StepKind::Transfer,
            6 => StepKind::Release,
            _ => StepKind::Standalone,
        }
    };
    let index = usize::try_from(position % pool_len.max(1) as u64).unwrap_or(0);

    Step {
        kind,
        index,
        variant,
        payload,
    }
}

enum PoolItem {
    Port(Port),
    Signal(Signal),
}

impl PoolItem {
    fn create(variant: u64) -> Result<Self, StepFailure> {
        if variant.is_multiple_of(2) {
            Port::create()
                .map(Self::Port)
                .map_err(|_| StepFailure::of("port_create", FailureKind::violated(-1800)))
        } else {
            Signal::create()
                .map(Self::Signal)
                .map_err(|_| StepFailure::of("signal_create", FailureKind::violated(-1801)))
        }
    }

    fn derive(&self, badge: NonZeroU64) -> Result<Self, StepFailure> {
        // Порту передаётся право слать, сигналу - право ждать. Производная того же вида,
        // что и её источник.
        match self {
            Self::Port(port) => port
                .duplicate(Rights::WRITE, Some(badge))
                .map(Self::Port)
                .map_err(|_| StepFailure::of("derive", FailureKind::violated(-1700))),
            Self::Signal(signal) => signal
                .duplicate(Rights::READ, Some(badge))
                .map(Self::Signal)
                .map_err(|_| StepFailure::of("derive", FailureKind::violated(-1701))),
        }
    }

    fn exercise(&self, sender: &Self, variant: u64, payload: u64) -> Result<(), StepFailure> {
        match (self, sender) {
            (Self::Port(port), Self::Port(client)) if variant.is_multiple_of(2) => {
                port_send_recv(port, client.as_raw(), payload)
                    .map_err(|kind| StepFailure::of("port_send_recv", kind))
            }
            (Self::Port(port), Self::Port(client)) => {
                port_call_reply(port, client.as_raw(), payload)
                    .map_err(|kind| StepFailure::of("port_call_reply", kind))
            }
            (Self::Signal(signal), Self::Signal(waiter)) => {
                signal_set_wait(signal, waiter.as_raw(), payload)
                    .map_err(|kind| StepFailure::of("signal_set_wait", kind))
            }
            _ => Err(StepFailure::of("exercise", FailureKind::violated(-1704))),
        }
    }

    fn drain_departure(&self, badge: NonZeroU64) -> Result<(), StepFailure> {
        match self {
            Self::Port(port) => drain_departed_badge(port, badge)
                .map_err(|kind| StepFailure::of("drain_departed_badge", kind)),
            Self::Signal(_) => Ok(()),
        }
    }
}

fn received_badge() -> u64 {
    let Some(buffer) = ipc_buffer_ptr() else {
        return 0;
    };
    // SAFETY: текущий поток единолично читает свой IPC-буфер после возврата из
    // recv; badge - поле той же выделенной ядром страницы.
    unsafe { (*buffer).badge }
}

fn drain_departed_badge(port: &Port, badge: NonZeroU64) -> Result<(), FailureKind> {
    // Порт существует дольше своей переданной стороны, и кадр её ухода приходит той же
    // очередью, что и сообщения: следующая операция над портом взяла бы его.
    match port.recv(OPERATION_TIMEOUT) {
        Err(Error::Syscall(SyscallError::SenderGone)) if received_badge() == badge.get() => Ok(()),
        Err(error) => Err(FailureKind::from_error(
            "drain",
            OPERATION_TIMEOUT,
            error,
            -1702,
        )),
        Ok(_) => Err(FailureKind::violated(-1703)),
    }
}

fn standalone(variant: u64, payload: u64) -> Result<(), StepFailure> {
    match variant % STANDALONE_OPERATIONS {
        0 => signal_close_wait(payload).map_err(|kind| StepFailure::of("signal_close_wait", kind)),
        1 => port_peer_close(payload).map_err(|kind| StepFailure::of("port_peer_close", kind)),
        _ => port_poll_timeout().map_err(|kind| StepFailure::of("port_poll_timeout", kind)),
    }
}

struct Pool {
    items: Vec<PoolItem>,
}

impl Pool {
    fn new() -> Self {
        Self {
            items: Vec::with_capacity(POOL_CAPACITY),
        }
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    fn apply(&mut self, step: Step) -> Result<(), StepFailure> {
        match step.kind {
            StepKind::Create => {
                if self.items.len() == POOL_CAPACITY {
                    self.items.remove(step.index);
                }
                self.items.push(PoolItem::create(step.variant)?);
                Ok(())
            }
            StepKind::Use => {
                let item = &self.items[step.index];
                item.exercise(item, step.variant, step.payload)
            }
            StepKind::Transfer => {
                let item = &self.items[step.index];
                let badge = NonZeroU64::MIN.saturating_add(step.payload);
                let derived = item.derive(badge)?;
                item.exercise(&derived, step.variant, step.payload)?;
                drop(derived);
                item.drain_departure(badge)
            }
            StepKind::Release => {
                self.items.remove(step.index);
                Ok(())
            }
            StepKind::Standalone => standalone(step.variant, step.payload),
        }
    }
}

fn remaining_budget() -> Result<u64, Error> {
    // Бисекция по `split`: он отрезает не меньше объявленного числа байт и
    // возвращает их закрытием производного ресурса. Замер не расходует бюджет и
    // не двигает измеряемую величину.
    let resource = Resource::self_resource()?;
    let mut attainable = 0;
    let mut refused = 1;

    loop {
        match resource.split(refused) {
            Ok(child) => drop(child),
            Err(Error::Syscall(SyscallError::ResourceExhausted)) => break,
            Err(error) => return Err(error),
        }
        attainable = refused;
        let Some(doubled) = refused.checked_mul(2) else {
            return Ok(attainable);
        };
        refused = doubled;
    }

    while refused - attainable > 1 {
        let middle = attainable + (refused - attainable) / 2;
        match resource.split(middle) {
            Ok(child) => {
                drop(child);
                attainable = middle;
            }
            Err(Error::Syscall(SyscallError::ResourceExhausted)) => refused = middle,
            Err(error) => return Err(error),
        }
    }

    Ok(attainable)
}

fn warm_up() {
    // Куча процесса растёт чанками и страницы обратно не отдаёт. Базовая
    // величина снимается после того, как прогон один раз занял всё, что
    // занимает эпоха: пул полной ёмкости, оба вида объектов и по шагу каждого
    // вида.
    let mut pool = Pool::new();
    let mut steps = Vec::new();
    for variant in 0..POOL_CAPACITY as u64 {
        steps.push(Step {
            kind: StepKind::Create,
            index: 0,
            variant,
            payload: variant,
        });
    }
    for index in 0..POOL_CAPACITY {
        let variant = index as u64;
        for kind in [StepKind::Use, StepKind::Transfer] {
            steps.push(Step {
                kind,
                index,
                variant,
                payload: variant,
            });
        }
    }
    for variant in 0..STANDALONE_OPERATIONS {
        steps.push(Step {
            kind: StepKind::Standalone,
            index: 0,
            variant,
            payload: variant,
        });
    }
    for _ in 0..POOL_CAPACITY {
        steps.push(Step {
            kind: StepKind::Release,
            index: 0,
            variant: 0,
            payload: 0,
        });
    }

    for (position, step) in steps.into_iter().enumerate() {
        if let Err(failure) = pool.apply(step) {
            report(format_args!("warm-up step={position}"), failure);
        }
    }
}

fn run_epoch(seed: u64, epoch: u64, steps: u64) {
    // Пул уходит с эпохой: всё, что она взяла, отдано до снятия величины.
    let derived = epoch_seed(seed, epoch);
    let mut random = SplitMix64::new(derived);
    let mut pool = Pool::new();

    for step in 0..steps {
        let chosen = choose(&mut random, pool.len());
        if let Err(failure) = pool.apply(chosen) {
            report(
                format_args!(
                    "seed={seed} epoch={epoch} epoch_seed={derived} step={step} kind={:?}",
                    chosen.kind
                ),
                failure,
            );
        }
    }
}

fn report(context: fmt::Arguments<'_>, failure: StepFailure) -> ! {
    // Истечение отведённого времени уходит отдельным маркером: xtask отличает
    // его от нарушенного утверждения.
    if failure.expired() {
        kernel_tests::runner::with_writer(|writer| {
            let mut output = kernel_tests::runner::FmtAdapter::new(writer);
            let _ = writeln!(output, "[STRESS-TIMEOUT: {context} {failure}]");
        });
    }
    kernel_tests::macros::fail(format_args!("stress {context} {failure}"), file!(), line!())
}

fn measure(context: fmt::Arguments<'_>) -> u64 {
    // Нехватка остатка на само снятие величины - то же расхождение.
    match remaining_budget() {
        Ok(budget) => budget,
        Err(error) => kernel_tests::macros::fail(
            format_args!("stress {context}: budget probe refused ({error:?})"),
            file!(),
            line!(),
        ),
    }
}

pub(crate) fn log_seed(seed: u64, steps: u64) {
    kernel_tests::runner::with_writer(|writer| {
        let mut output = kernel_tests::runner::FmtAdapter::new(writer);
        let _ = writeln!(output, "[STRESS-START: seed={seed} steps={steps}]");
    });
}

pub(crate) fn log_pass(seed: u64) {
    kernel_tests::runner::with_writer(|writer| {
        let mut output = kernel_tests::runner::FmtAdapter::new(writer);
        // Форму маркера разбирает xtask, считая пройденные seed прогона.
        let _ = writeln!(output, "[STRESS-PASS: seed={seed}]");
    });
}

fn log_base(budget: u64) {
    kernel_tests::runner::with_writer(|writer| {
        let mut output = kernel_tests::runner::FmtAdapter::new(writer);
        let _ = writeln!(output, "[STRESS-BASE: budget={budget}]");
    });
}

fn log_epoch(seed: u64, epoch: u64, budget: u64) {
    kernel_tests::runner::with_writer(|writer| {
        let mut output = kernel_tests::runner::FmtAdapter::new(writer);
        // Форму маркера разбирает xtask, считая отчитавшиеся эпохи прогона.
        let _ = writeln!(
            output,
            "[STRESS-EPOCH: seed={seed} epoch={epoch} budget={budget}]"
        );
    });
}

#[kernel_test(stress)]
fn port_signal_lifecycle_stress() {
    let plan = StressPlan::load();
    let epochs = plan.epochs();

    warm_up();
    let base = measure(format_args!("before the first epoch"));
    log_base(base);

    for seed in plan.seeds() {
        log_seed(seed, plan.steps);
        for epoch in 0..epochs {
            run_epoch(seed, epoch, plan.epoch_length(epoch));
            let budget = measure(format_args!("seed={seed} epoch={epoch}"));
            kernel_tests::kassert!(
                budget == base,
                "stress seed={seed} epoch={epoch}: budget base={base} now={budget}"
            );
            log_epoch(seed, epoch, budget);
        }
        log_pass(seed);
    }
}

#[kernel_test]
fn plan_parses_declared_fields() {
    let plan = StressPlan::parse("seed=7\nseeds=3\nsteps=64\nepoch=16\n").expect("plan parses");
    kernel_tests::kassert_eq!(plan.steps, 64);
    kernel_tests::kassert_eq!(plan.epochs(), 4);
    let seeds: alloc::vec::Vec<u64> = plan.seeds().collect();
    kernel_tests::kassert_eq!(seeds, alloc::vec![7, 8, 9]);
}

#[kernel_test]
fn epochs_cover_the_steps_of_a_seed() {
    let plan = StressPlan::parse("seed=1\nseeds=1\nsteps=70\nepoch=32\n").expect("plan parses");
    kernel_tests::kassert_eq!(plan.epochs(), 3);
    let lengths: alloc::vec::Vec<u64> = (0..plan.epochs()).map(|e| plan.epoch_length(e)).collect();
    kernel_tests::kassert_eq!(lengths, alloc::vec![32, 32, 6]);
    kernel_tests::kassert_eq!(lengths.iter().sum::<u64>(), plan.steps);
}

#[kernel_test]
fn plan_rejects_an_unknown_key() {
    kernel_tests::kassert_eq!(
        StressPlan::parse("seed=1\nsteps=2\nseeds=3\nepoch=1\ndepth=4\n").err(),
        Some(PlanError::Unknown("depth"))
    );
}

#[kernel_test]
fn plan_rejects_a_repeated_key() {
    kernel_tests::kassert_eq!(
        StressPlan::parse("seed=1\nseed=2\nseeds=3\nsteps=4\nepoch=1\n").err(),
        Some(PlanError::Repeated("seed"))
    );
}

#[kernel_test]
fn plan_rejects_a_value_that_is_not_a_number() {
    kernel_tests::kassert_eq!(
        StressPlan::parse("seed=1\nseeds=many\nsteps=4\nepoch=1\n").err(),
        Some(PlanError::NotANumber("seeds"))
    );
}

#[kernel_test]
fn plan_rejects_a_line_without_a_separator() {
    kernel_tests::kassert_eq!(
        StressPlan::parse("seed=1\nseeds\nsteps=4\nepoch=1\n").err(),
        Some(PlanError::Malformed("seeds"))
    );
}

#[kernel_test]
fn plan_rejects_a_zero_count() {
    kernel_tests::kassert_eq!(
        StressPlan::parse("seed=1\nseeds=0\nsteps=4\nepoch=1\n").err(),
        Some(PlanError::Zero("seeds"))
    );
    kernel_tests::kassert_eq!(
        StressPlan::parse("seed=1\nseeds=2\nsteps=0\nepoch=1\n").err(),
        Some(PlanError::Zero("steps"))
    );
    kernel_tests::kassert_eq!(
        StressPlan::parse("seed=1\nseeds=2\nsteps=4\nepoch=0\n").err(),
        Some(PlanError::Zero("epoch"))
    );
}

#[kernel_test]
fn plan_rejects_an_incomplete_declaration() {
    kernel_tests::kassert_eq!(
        StressPlan::parse("seeds=2\nsteps=4\nepoch=1\n").err(),
        Some(PlanError::Missing("seed"))
    );
    kernel_tests::kassert_eq!(
        StressPlan::parse("seed=1\nseeds=2\nsteps=4\n").err(),
        Some(PlanError::Missing("epoch"))
    );
    kernel_tests::kassert_eq!(
        StressPlan::parse("").err(),
        Some(PlanError::Missing("seed"))
    );
}

#[kernel_test]
fn the_first_epoch_runs_the_seed_of_the_run() {
    kernel_tests::kassert_eq!(epoch_seed(0x1234_5678, 0), 0x1234_5678);
    kernel_tests::kassert_ne!(epoch_seed(0x1234_5678, 1), 0x1234_5678);
}

#[kernel_test]
fn the_same_seed_repeats_the_step_sequence() {
    const SEQUENCE_STEPS: usize = 64;

    fn sequence(seed: u64) -> alloc::vec::Vec<Step> {
        let mut random = SplitMix64::new(seed);
        let mut length = 0;
        let mut steps = alloc::vec::Vec::new();
        for _ in 0..SEQUENCE_STEPS {
            let step = choose(&mut random, length);
            length = match step.kind {
                StepKind::Create => (length + 1).min(POOL_CAPACITY),
                StepKind::Release => length - 1,
                _ => length,
            };
            steps.push(step);
        }
        steps
    }

    let sequence_of_the_seed = sequence(0x51ED);
    kernel_tests::kassert_eq!(sequence_of_the_seed, sequence(0x51ED));

    // Освобождение идёт по выбору генератора. Элемент, созданный раньше, освобождается
    // позже созданного после него.
    kernel_tests::kassert!(
        sequence_of_the_seed
            .iter()
            .any(|step| step.kind == StepKind::Release && step.index > 0)
    );
}

#[kernel_test]
fn an_operation_out_of_time_is_not_a_violated_assertion() {
    let port = Port::create().expect("port of the probe");
    let refusal = port
        .recv(Timeout::POLL)
        .expect_err("a port without a sender refuses a poll");

    let kind = FailureKind::from_error("recv", Timeout::POLL, refusal, -1102);
    kernel_tests::kassert!(kind.expired());
    kernel_tests::kassert_eq!(
        alloc::format!("{kind}"),
        alloc::string::String::from("outcome=expired phase=recv timeout_ns=0")
    );
}

#[kernel_test]
fn a_refused_operation_is_not_an_expiry() {
    let signal = Signal::create().expect("signal of the probe");
    let blind = signal
        .duplicate(Rights::WRITE, None)
        .expect("a derived handle without READ");

    let failure =
        signal_set_wait(&signal, blind.as_raw(), 1).expect_err("waiting without READ is refused");
    kernel_tests::kassert!(!failure.expired());
}
