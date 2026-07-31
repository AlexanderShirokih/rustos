//! Воспроизводимый userspace stress-прогон Port, Signal и lifecycle через syscall ABI.

use alloc::sync::Arc;
use core::{
    fmt::Write as _,
    sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::SeqCst},
};

use kernel_tests::kernel_test;
use runtime::{
    JoinHandle, Port, Priority, Timeout, handle_close, ipc_buffer_ptr, port_call, port_send,
    signal_create, signal_set, signal_wait_one, spawn,
};
use syscall::{Handle, Rights, SIGNALED, SyscallError, WakeCount, decode_tag, encode_tag};

const STACK_SIZE: u64 = 0x4000;
const OPERATION_TIMEOUT_NS: u64 = 1_000_000_000;
const JOIN_TIMEOUT_NS: u64 = 2_000_000_000;
const READY_SPIN_LIMIT: usize = 10_000_000;

struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }
}

struct WorkerState {
    handle: u32,
    payload: u64,
    ready: AtomicBool,
    result: AtomicI64,
    observed: AtomicU64,
}

impl WorkerState {
    fn new(handle: Handle, payload: u64) -> Arc<Self> {
        Arc::new(Self {
            handle: handle.raw(),
            payload,
            ready: AtomicBool::new(false),
            result: AtomicI64::new(i64::MIN),
            observed: AtomicU64::new(0),
        })
    }

    fn handle(&self) -> Option<Handle> {
        Handle::new(self.handle)
    }
}

fn worker_state(arg: usize) -> Arc<WorkerState> {
    // SAFETY: arg получен из Arc::into_raw в spawn_worker ровно для этого потока;
    // from_raw забирает ту самую ссылку и освобождает её на выходе из worker.
    unsafe { Arc::from_raw(arg as *const WorkerState) }
}

fn spawn_worker(
    entry: extern "C" fn(usize) -> u32,
    state: &Arc<WorkerState>,
    spawn_error: i64,
) -> Result<JoinHandle, i64> {
    let arg = Arc::into_raw(Arc::clone(state)) as usize;
    spawn(entry, arg, STACK_SIZE, Priority::new(1)).map_err(|_| {
        // SAFETY: поток не стартовал, ссылку из into_raw забрать больше некому.
        drop(unsafe { Arc::from_raw(arg as *const WorkerState) });
        spawn_error
    })
}

fn write_payload(payload: u64) -> bool {
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

fn read_payload() -> Option<u64> {
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
    let state = worker_state(arg);
    state.ready.store(true, SeqCst);
    if !write_payload(state.payload) {
        state.result.store(-1000, SeqCst);
        return 0;
    }
    let result = state
        .handle()
        .map_or(-1001, |handle| port_send(handle, OPERATION_TIMEOUT_NS));
    state.result.store(result, SeqCst);
    0
}

extern "C" fn call_worker(arg: usize) -> u32 {
    let state = worker_state(arg);
    state.ready.store(true, SeqCst);
    if !write_payload(state.payload) {
        state.result.store(-1000, SeqCst);
        return 0;
    }
    let result = state
        .handle()
        .map_or(-1001, |handle| port_call(handle, OPERATION_TIMEOUT_NS));
    if result == 0 {
        state.observed.store(read_payload().unwrap_or(0), SeqCst);
    }
    state.result.store(result, SeqCst);
    0
}

extern "C" fn signal_wait_worker(arg: usize) -> u32 {
    let state = worker_state(arg);
    state.ready.store(true, SeqCst);
    let result = state.handle().map_or(-1001, |handle| {
        signal_wait_one(handle, SIGNALED, OPERATION_TIMEOUT_NS)
    });
    state.result.store(result, SeqCst);
    0
}

fn wait_until_ready(state: &WorkerState) -> Result<(), i64> {
    for _ in 0..READY_SPIN_LIMIT {
        if state.ready.load(SeqCst) {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(-1003)
}

fn join(worker: JoinHandle) -> Result<(), i64> {
    worker
        .join(Timeout::from_ns(JOIN_TIMEOUT_NS))
        .map_err(|_| -1002)
}

fn send_recv(payload: u64) -> Result<(), i64> {
    let port = Port::create().map_err(|_| -1100)?;
    let state = WorkerState::new(port.handle().as_raw(), payload);
    let worker = spawn_worker(send_worker, &state, -1101)?;
    wait_until_ready(&state)?;

    let mut bytes = [0u8; size_of::<u64>()];
    let (len, reply) = port
        .recv_bytes(&mut bytes, Timeout::from_ns(OPERATION_TIMEOUT_NS))
        .map_err(|_| -1102)?;
    if len != bytes.len() || reply.is_some() || u64::from_le_bytes(bytes) != payload {
        return Err(-1103);
    }
    join(worker)?;
    match state.result.load(SeqCst) {
        0 => Ok(()),
        result => Err(result),
    }
}

fn call_reply(payload: u64) -> Result<(), i64> {
    let port = Port::create().map_err(|_| -1200)?;
    let state = WorkerState::new(port.handle().as_raw(), payload);
    let worker = spawn_worker(call_worker, &state, -1201)?;
    wait_until_ready(&state)?;

    let mut bytes = [0u8; size_of::<u64>()];
    let (len, reply) = port
        .recv_bytes(&mut bytes, Timeout::from_ns(OPERATION_TIMEOUT_NS))
        .map_err(|_| -1202)?;
    let Some(reply) = reply else {
        return Err(-1203);
    };
    if len != bytes.len() || u64::from_le_bytes(bytes) != payload {
        return Err(-1204);
    }
    let response = payload.rotate_left(17);
    reply
        .reply_bytes(&response.to_le_bytes())
        .map_err(|_| -1205)?;
    join(worker)?;
    match state.result.load(SeqCst) {
        0 if state.observed.load(SeqCst) == response => Ok(()),
        0 => Err(-1206),
        result => Err(result),
    }
}

fn signal_set_wait(payload: u64) -> Result<(), i64> {
    let signal = signal_create().map_err(|_| -1300)?;
    let state = WorkerState::new(signal, payload);
    let worker = spawn_worker(signal_wait_worker, &state, -1301)?;
    wait_until_ready(&state)?;

    let set_result = signal_set(signal, SIGNALED, 0, WakeCount::All);
    if set_result != 0 {
        return Err(set_result);
    }
    join(worker)?;
    let wait_result = state.result.load(SeqCst);
    let close_result = handle_close(signal);
    if close_result != 0 {
        return Err(close_result);
    }
    if wait_result == i64::from(SIGNALED) {
        Ok(())
    } else {
        Err(wait_result)
    }
}

fn signal_close_wait(payload: u64) -> Result<(), i64> {
    let signal = signal_create().map_err(|_| -1400)?;
    let state = WorkerState::new(signal, payload);
    let worker = spawn_worker(signal_wait_worker, &state, -1401)?;
    wait_until_ready(&state)?;

    let close_result = handle_close(signal);
    if close_result != 0 {
        return Err(close_result);
    }
    join(worker)?;
    let result = state.result.load(SeqCst);
    match SyscallError::from_return(result) {
        Some(SyscallError::Canceled | SyscallError::BadHandle) => Ok(()),
        _ => Err(result),
    }
}

fn port_peer_close(payload: u64) -> Result<(), i64> {
    let port = Port::create().map_err(|_| -1500)?;
    let client = port
        .handle()
        .duplicate(Rights::WRITE, payload | 1)
        .map_err(|_| -1501)?;
    let state = WorkerState::new(client.as_raw(), payload);
    let worker = spawn_worker(send_worker, &state, -1502)?;
    wait_until_ready(&state)?;

    drop(port);
    join(worker)?;
    let result = state.result.load(SeqCst);
    match SyscallError::from_return(result) {
        Some(SyscallError::PeerClosed | SyscallError::Revoked) => Ok(()),
        _ => Err(result),
    }
}

fn port_poll_timeout() -> Result<(), i64> {
    let port = Port::create().map_err(|_| -1600)?;
    match port.recv(Timeout::POLL) {
        Err(runtime::Error::Syscall(SyscallError::Timeout)) => Ok(()),
        _ => Err(-1601),
    }
}

fn log_seed(seed: u64, steps: u64) {
    kernel_tests::runner::with_writer(|writer| {
        let mut output = kernel_tests::runner::FmtAdapter::new(writer);
        let _ = writeln!(output, "[STRESS-START: seed={seed} steps={steps}]");
    });
}

#[kernel_test(stress)]
fn port_signal_lifecycle_stress() {
    let first_seed = env!("RUSTOS_STRESS_SEED").parse::<u64>().unwrap_or(0);
    let seed_count = env!("RUSTOS_STRESS_SEEDS").parse::<u64>().unwrap_or(0);
    let steps = env!("RUSTOS_STRESS_STEPS").parse::<u64>().unwrap_or(0);
    kernel_tests::kassert!(
        seed_count > 0,
        "stress seed count must be a positive number"
    );
    kernel_tests::kassert!(steps > 0, "stress step count must be a positive number");

    for seed_offset in 0..seed_count {
        let seed = first_seed.wrapping_add(seed_offset);
        log_seed(seed, steps);
        let mut random = SplitMix64::new(seed);
        for step in 0..steps {
            let payload = random.next();
            let (operation, result) = match random.next() % 6 {
                0 => ("send_recv", send_recv(payload)),
                1 => ("call_reply", call_reply(payload)),
                2 => ("signal_set_wait", signal_set_wait(payload)),
                3 => ("signal_close_wait", signal_close_wait(payload)),
                4 => ("port_peer_close", port_peer_close(payload)),
                _ => ("port_poll_timeout", port_poll_timeout()),
            };
            kernel_tests::kassert!(
                result.is_ok(),
                "stress seed={seed} step={step} operation={operation} result={result:?}"
            );
        }
        kernel_tests::runner::with_writer(|writer| {
            let mut output = kernel_tests::runner::FmtAdapter::new(writer);
            let _ = writeln!(output, "[STRESS-PASS: seed={seed}]");
        });
    }
}
