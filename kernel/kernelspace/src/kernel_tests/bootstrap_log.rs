//! bootstrap-log: round-trip `Bootstrap.log` через синхронный Port.

use alloc::sync::Arc;

use bootstrap::{BootstrapClient, BootstrapService, LOG_MESSAGE_MAX, dispatch_bootstrap};
use collections::{LockCell, MutexCell};
use ipc::{
    MessageLen, Transport,
    wire::{IpcError as WireError, Str},
};
use kernel_tests::kernel_test;
use kobject::{
    Handle, HandleTable, IpcError as KernelIpcError, KObject, KernelIpcBuffer, Port, Rights,
    SIGNALED, Signal, ThreadTransport, install_handle, port_send, runtime, signal_wait_one,
};
use scheduler::{Priority, SchedulerServiceExt, SpawnConfig};
use syscall::{IpcBuffer, decode_tag, encode_tag};

/// Серверный sink: зеркалит лог и сигналит Signal.
struct SignalingSink {
    signal: Arc<Signal>,
}

impl BootstrapService for SignalingSink {
    fn log(&mut self, _message: Str<{ LOG_MESSAGE_MAX }>) {
        self.signal.signal(SIGNALED, 0);
    }
}

/// Минимальный kernel-транспорт клиента: `#[cast]` -> `port_send` поверх
/// kernel-резидентного IPC-буфера.
struct KernelClientTransport {
    port: Arc<Port>,
    buffer: KernelIpcBuffer,
    table: Arc<MutexCell<HandleTable>>,
}

impl Transport for KernelClientTransport {
    fn write_message(&self, bytes: &[u8], _handles: &[u32]) -> Result<(), WireError> {
        self.buffer.with_lock(|buf| {
            let len = bytes.len().min(buf.data.len());
            buf.data[..len].copy_from_slice(&bytes[..len]);
            buf.tag = encode_tag(len, 0);
        });
        let sender = ThreadTransport::new_kernel(self.buffer.clone(), self.table.clone());
        port_send(&self.port, sender, runtime(), None).map_err(map_err)
    }

    fn read_message(&self, _b: &mut [u8], _h: &mut [u32]) -> Result<MessageLen, WireError> {
        Err(WireError::WouldBlock)
    }

    fn wait_readable(&self, _t: u64) -> Result<(), WireError> {
        Ok(())
    }
}

/// Серверный kernel-транспорт.
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
        let _ = kobject::port_recv(&self.port, receiver, runtime(), None).map_err(map_err)?;
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

#[kernel_test]
fn bootstrap_log_round_trip_over_port() {
    let port = Port::new();

    let signal = Signal::new();
    let signal_id = install_handle(Handle::new(KObject::Signal(signal.clone()), Rights::READ))
        .expect("install signal handle");

    // Серверный логгер-таск: один recv+dispatch, sink сигналит доставку.
    let scheduler = super::scheduler().clone();
    let server_port = port.clone();
    let server_signal = signal.clone();
    scheduler
        .spawn(
            SpawnConfig::new("bootstrap-log-server").priority(Priority::normal()),
            move || {
                let table = runtime()
                    .current_handle_table()
                    .expect("server kernel handle-table");
                let transport = KernelServerTransport {
                    port: server_port,
                    buffer: Arc::new(MutexCell::new(IpcBuffer::zeroed())),
                    table,
                };
                let mut sink = SignalingSink {
                    signal: server_signal,
                };
                dispatch_bootstrap(&mut sink, &transport).expect("dispatch log frame");
            },
        )
        .expect("server task spawn");

    // Клиентский таск: шлёт log синхронно.
    let scheduler2 = super::scheduler().clone();
    let client_port = port.clone();
    scheduler2
        .spawn(
            SpawnConfig::new("bootstrap-log-client").priority(Priority::normal()),
            move || {
                let table = runtime()
                    .current_handle_table()
                    .expect("client kernel handle-table");
                let transport = KernelClientTransport {
                    port: client_port,
                    buffer: Arc::new(MutexCell::new(IpcBuffer::zeroed())),
                    table,
                };
                let client = BootstrapClient::new(transport);
                client
                    .log(Str::<LOG_MESSAGE_MAX>::new("boot ok").expect("log fits"))
                    .expect("log send");
            },
        )
        .expect("client task spawn");

    let observed = signal_wait_one(signal_id, SIGNALED, Some(5_000_000_000))
        .expect("log must be delivered and dispatched");
    kernel_tests::kassert!(observed & SIGNALED != 0);
}
