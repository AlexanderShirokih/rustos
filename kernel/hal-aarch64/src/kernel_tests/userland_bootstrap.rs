//! E2E полной цепочки userland: initrd-blob -> production-путь
//! `kernelspace::bootstrap::spawn_process` -> `Bootstrap::log` из EL0 через
//! синхронный Port -> ожидание завершения потока.
//!
//! Bootstrap-процесс (rootkeeper) шлёт `log("rootkeeper started")` через
//! `port_send` (блокирует до встречи) и сразу выходит. Тест выступает
//! kernel-получателем: `port_recv` матчит отправителя, декодирует кадр
//! контрактом `Bootstrap`, затем ждёт завершения bootstrap-потока.

use alloc::sync::Arc;

use bootstrap::{BootstrapService, dispatch_bootstrap};
use capability::{
    Capability, CapabilityTarget, HandleTable, IpcError as KernelIpcError, KernelIpcBuffer, Port,
    Rights, SIGNALED, ThreadTransport, install_handle, port_recv, runtime, signal_wait_one,
    thread_termination_signal,
};
use collections::{LockCell, MutexCell};
use ipc::{
    MessageLen, Transport,
    wire::{IpcError as WireError, Str},
};
use kernel_tests::kernel_test;
use kernelspace::bootstrap::BootstrapLaunch;
use scheduler::ArchContext;
use syscall::{IpcBuffer, decode_tag};

use crate::sched::Aarch64Context;

/// Bounded-бюджет ожидания как детектор провала: хэппи-пас
/// будится сигналом за миллисекунды.
const WAIT_BUDGET_NS: u64 = 5_000_000_000;

/// Серверный kernel-транспорт поверх kernel-резидентного IPC-буфера и
/// kernel port API: `read_message` -> `port_recv` + декод.
struct KernelServerTransport {
    port: Arc<Port>,
    buffer: KernelIpcBuffer,
    table: Arc<MutexCell<HandleTable>>,
}

impl Transport for KernelServerTransport {
    fn write_message(&self, _b: &[u8], _h: &[u32]) -> Result<(), WireError> {
        Err(WireError::PeerClosed)
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, WireError> {
        let receiver = ThreadTransport::new_kernel(self.buffer.clone(), self.table.clone());
        let _ = port_recv(&self.port, receiver, runtime(), None).map_err(map_err)?;
        self.buffer.with_lock(|buf| load(buf, bytes, handles))
    }

    fn wait_readable(&self, _t: u64) -> Result<(), WireError> {
        Ok(())
    }
}

fn map_err(e: KernelIpcError) -> WireError {
    match e {
        KernelIpcError::ShouldWait => WireError::WouldBlock,
        KernelIpcError::Timeout => WireError::Timeout,
        KernelIpcError::BufferTooSmall => WireError::Truncated,
        KernelIpcError::MessageTooBig => WireError::FrameOverflow,
        _ => WireError::PeerClosed,
    }
}

fn load(buf: &IpcBuffer, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, WireError> {
    let (len, ncaps) = decode_tag(buf.tag);
    let len = len.min(buf.data.len());
    let ncaps = ncaps.min(buf.caps.len());
    if len > bytes.len() || ncaps > handles.len() {
        return Err(WireError::Truncated);
    }
    bytes[..len].copy_from_slice(&buf.data[..len]);
    for (dst, &raw) in handles.iter_mut().zip(&buf.caps[..ncaps]) {
        *dst = raw;
    }
    Ok(MessageLen::new(len, ncaps))
}

struct LogProbe {
    received: bool,
}

impl BootstrapService for LogProbe {
    fn log(&mut self, message: Str<{ bootstrap::LOG_MESSAGE_MAX }>) {
        self.received = message.as_str() == "rootkeeper started";
    }
}

fn spawn_bootstrap_with_log() -> BootstrapLaunch {
    let blob = kernelspace::kernel_tests::userland_blob()
        .expect("userland blob must be present in test mode");

    let launch = kernelspace::bootstrap::spawn_process(
        kernelspace::kernel_tests::user_process_launcher().as_ref(),
        blob,
        Aarch64Context::USER_VA_END,
    )
    .expect("spawn_process must succeed");
    // initial handle[0] = bootstrap-port, initial handle[1] = корневой
    // Resource (полномочие на минтинг физпамяти + бюджет).
    kernel_tests::kassert_eq!(launch.info.initial_handle_ids.len(), 2);

    // Kernel-получатель: recv матчит синхронный send bootstrap-процесса.
    let table = runtime()
        .current_handle_table()
        .expect("kernel handle-table");
    let transport = KernelServerTransport {
        port: launch.port.clone(),
        buffer: Arc::new(MutexCell::new(IpcBuffer::zeroed())),
        table,
    };
    let mut probe = LogProbe { received: false };
    dispatch_bootstrap(&mut probe, &transport).expect("bootstrap log must dispatch");
    kernel_tests::kassert!(probe.received);

    launch
}

/// Bootstrap-поток уже отправил лог (send вернулся после нашего recv) и
/// выходит сам; ждём завершения через его bound-`Signal`.
fn teardown_bootstrap(launch: BootstrapLaunch) {
    let thread_id = install_handle(Capability::new(
        CapabilityTarget::Thread(launch.info.thread_object),
        Rights::READ,
    ))
    .expect("install bootstrap thread handle");
    let term_signal =
        thread_termination_signal(thread_id).expect("bootstrap thread termination signal");
    let observed = signal_wait_one(term_signal, SIGNALED, Some(WAIT_BUDGET_NS))
        .expect("wait for bootstrap exit");
    kernel_tests::kassert!(observed & SIGNALED != 0);
}

#[kernel_test]
fn userland_bootstrap_log() {
    let launch = spawn_bootstrap_with_log();
    teardown_bootstrap(launch);
}
