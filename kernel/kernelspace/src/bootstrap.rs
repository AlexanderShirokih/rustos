//! Production-цепочка userland: запуск первого userland-процесса с bootstrap-портом.

extern crate alloc;

use alloc::{sync::Arc, vec};
use core::num::NonZeroUsize;

use bootstrap::{BootstrapService, dispatch_bootstrap};
use capability::{
    Capability, CapabilityTarget, HandleTable, IpcError as KernelIpcError, IrqControl,
    KernelIpcBuffer, Port, Reply, Resource, ThreadTransport, port_recv, runtime,
};
use collections::{LockCell, MutexCell};
use ipc::{
    MessageLen, Transport,
    wire::{Cap, IpcError as WireError, Str},
};
use klog::{info, warn};
use memory::{AccessMask, MemoryRegion, page_round_up, physical_address::PageAlignedAddress};
use process::{UserImageFromModelError, user_image_parts_from_entry};
use scheduler::{Priority, UserProcessLaunch, UserProcessLaunchInfo};
use syscall::{IpcBuffer, decode_tag, encode_tag};
use userland::EntryView;
use userland_image::{ImageDecodeError, decode};

use crate::user_process::{SpawnUserError, UserProcessLauncher};

/// Диапазон корневого Resource: всё адресное пространство, выровненное вниз
/// до страницы.
const ROOT_RESOURCE_SPAN: NonZeroUsize = NonZeroUsize::new(usize::MAX & !0xFFF).unwrap();

/// Запущенный bootstrap-процесс.
pub struct BootstrapLaunch {
    pub port: Arc<Port>,
    pub info: UserProcessLaunchInfo,
    pub irq_control: Arc<IrqControl>,
    pub image_region: Arc<MemoryRegion>,
}

/// Ошибки запуска bootstrap-процесса.
#[derive(Debug)]
pub enum BootstrapSpawnError {
    Image(ImageDecodeError),
    Model(UserImageFromModelError),
    Spawn(SpawnUserError),
}

/// Разбирает `blob` и спавнит процесс. `image_phys` - физбаза `blob` в initrd
/// (4K-выровнена), поверх которой выдаётся read-only регион образа.
pub fn spawn_process(
    launcher: &dyn UserProcessLauncher,
    blob: &'static [u8],
    image_phys: PageAlignedAddress,
    user_va_end: usize,
) -> Result<BootstrapLaunch, BootstrapSpawnError> {
    let image = decode(blob).map_err(BootstrapSpawnError::Image)?;
    let entry = image.bootstrap_entry();
    let name = entry.name();
    let parts =
        user_image_parts_from_entry(&entry, user_va_end).map_err(BootstrapSpawnError::Model)?;
    let user_image = parts.image();

    // Ядро держит Arc как получатель; Handle на тот же объект уходит bootstrap-процессу
    // как initial handle.
    let port = Port::new();
    let peer_handle = Capability::new_with_default_rights(port.clone());

    // Корневой Resource: полномочие на минтинг физпамяти.
    let root_resource = Resource::new(
        PageAlignedAddress::ZERO,
        ROOT_RESOURCE_SPAN,
        AccessMask::RW,
        1 << 20,
    );

    // Корневое полномочие на IRQ-линии: весь SPI-диапазон.
    let irq_control = IrqControl::new(32, 1019);

    // Read-only Normal-регион поверх байт образа; размер округлён до страницы
    // для маппинга. blob непуст (decode прошёл), поэтому размер ненулевой.
    let blob_len = NonZeroUsize::new(blob.len()).expect("decoded image is non-empty");
    let image_size = page_round_up(blob_len).expect("image size fits address space");
    let image_region = Arc::new(MemoryRegion::create_physical_normal(
        image_phys,
        image_size,
        AccessMask::R,
    ));

    let launch = UserProcessLaunch::new()
        .initial_handles(vec![peer_handle])
        .bootstrap_handle(0);

    let info = launcher
        .spawn_user_process_with_launch(name, &user_image, Priority::normal(), 2, launch)
        .map_err(BootstrapSpawnError::Spawn)?;

    // Корневой Resource - метеринг-ресурс bootstrap-процесса; ставится до старта scheduler.
    info.process_object.set_metering_resource(root_resource);

    Ok(BootstrapLaunch {
        port,
        info,
        irq_control,
        image_region,
    })
}

/// Цикл сервера bootstrap-port'а: kernel-поток аллоцирует kernel-резидентный
/// IPC-буфер, строит kernel-транспорт и в бесконечном цикле обслуживает контракт
/// `Bootstrap`.
pub fn run_bootstrap(
    port: &Arc<Port>,
    irq_control: Arc<IrqControl>,
    image_region: Arc<MemoryRegion>,
) {
    let Some(table) = runtime().current_handle_table() else {
        warn!("bootstrap: no kernel handle-table; server not started");
        return;
    };

    let buffer: KernelIpcBuffer = Arc::new(MutexCell::new(IpcBuffer::zeroed()));
    let transport = KernelPortTransport::new(port.clone(), buffer, table.clone());
    let mut server = BootstrapServer {
        table,
        irq_control,
        image_region,
    };

    loop {
        match dispatch_bootstrap(&mut server, &transport) {
            Ok(()) => {}
            Err(WireError::PeerClosed) => {
                info!("bootstrap: port closed, exiting");
                return;
            }
            Err(e) => {
                warn!("bootstrap: dispatch failed: {:?}", e);
                return;
            }
        }
    }
}

/// Код ошибки выдачи в bootstrap-протоколе: отказ вставки дубликата в таблицу.
const ACQUIRE_FAILED: u32 = 1;

/// Серверная сторона контракта `Bootstrap`.
struct BootstrapServer {
    table: Arc<MutexCell<HandleTable>>,
    irq_control: Arc<IrqControl>,
    image_region: Arc<MemoryRegion>,
}

impl BootstrapServer {
    /// Кладёт дубликат `target` в таблицу сервера, возвращает его индекс как
    /// `Cap`; перенос вызывателю делает reply-путь транспорта.
    fn vend(&self, target: CapabilityTarget) -> Result<Cap, u32> {
        let cap = Capability::new_with_default_rights(target);
        let id = self
            .table
            .with_lock(|tbl| tbl.insert(cap))
            .map_err(|_| ACQUIRE_FAILED)?;

        Ok(Cap::from_raw(id.raw()))
    }
}

impl BootstrapService for BootstrapServer {
    fn log(&mut self, message: Str<{ bootstrap::LOG_MESSAGE_MAX }>) {
        info!("{}", message.as_str());
    }

    fn acquire_irq_control(&mut self) -> Result<Cap, u32> {
        self.vend(CapabilityTarget::IrqControl(self.irq_control.clone()))
    }

    fn acquire_userland_image(&mut self) -> Result<Cap, u32> {
        self.vend(CapabilityTarget::Memory(self.image_region.clone()))
    }
}

/// Сводит kernel-ошибку IPC к ошибке wire-транспорта.
fn map_kernel_error(error: KernelIpcError) -> WireError {
    match error {
        KernelIpcError::ShouldWait => WireError::WouldBlock,
        KernelIpcError::Timeout => WireError::Timeout,
        KernelIpcError::BufferTooSmall => WireError::Truncated,
        KernelIpcError::MessageTooBig => WireError::FrameOverflow,
        KernelIpcError::PeerClosed
        | KernelIpcError::BadHandle
        | KernelIpcError::WrongType
        | KernelIpcError::AccessDenied
        | KernelIpcError::Canceled
        | KernelIpcError::OutOfHandles
        | KernelIpcError::ResourceExhausted
        | KernelIpcError::Revoked => WireError::PeerClosed,
    }
}

/// Kernel-side port-транспорт серверной роли поверх kernel-резидентного
/// IPC-буфера и kernel port API.
///
/// Реализует только то, что нужно `dispatch_bootstrap`.
struct KernelPortTransport {
    port: Arc<Port>,
    buffer: KernelIpcBuffer,
    table: Arc<MutexCell<HandleTable>>,
    pending_reply: MutexCell<Option<Arc<Reply>>>,
}

impl KernelPortTransport {
    fn new(port: Arc<Port>, buffer: KernelIpcBuffer, table: Arc<MutexCell<HandleTable>>) -> Self {
        Self {
            port,
            buffer,
            table,
            pending_reply: MutexCell::new(None),
        }
    }

    fn thread_transport(&self) -> ThreadTransport {
        ThreadTransport::new_kernel(self.buffer.clone(), self.table.clone())
    }
}

impl Transport for KernelPortTransport {
    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), WireError> {
        let Some(reply) = self.pending_reply.with_lock(Option::take) else {
            warn!("bootstrap: write_message without a pending reply");
            return Ok(());
        };

        let delivered = self
            .buffer
            .with_lock(|buf| store_kernel_buffer(buf, bytes, handles))
            .and_then(|()| {
                reply
                    .reply(&self.thread_transport())
                    .map_err(map_kernel_error)
            });

        if let Err(e) = delivered {
            warn!("bootstrap: reply delivery failed: {:?}", e);
            reply.cancel();
        }
        Ok(())
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, WireError> {
        let receiver = self.thread_transport();
        let reply = port_recv(&self.port, receiver, runtime(), None).map_err(map_kernel_error)?;
        self.pending_reply.with_lock(|slot| *slot = reply);
        self.buffer
            .with_lock(|buf| load_kernel_buffer(buf, bytes, handles))
    }

    fn wait_readable(&self, _timeout_ns: u64) -> Result<(), WireError> {
        // recv сам блокирует в read_message.
        Ok(())
    }
}

fn store_kernel_buffer(
    buf: &mut IpcBuffer,
    bytes: &[u8],
    handles: &[u32],
) -> Result<(), WireError> {
    if bytes.len() > buf.data.len() || handles.len() > buf.caps.len() {
        return Err(WireError::FrameOverflow);
    }
    buf.data[..bytes.len()].copy_from_slice(bytes);
    for (slot, &handle) in buf.caps.iter_mut().zip(handles) {
        *slot = handle;
    }
    buf.tag = encode_tag(bytes.len(), handles.len());
    Ok(())
}

fn load_kernel_buffer(
    buf: &IpcBuffer,
    bytes: &mut [u8],
    handles: &mut [u32],
) -> Result<MessageLen, WireError> {
    let decoded = decode_tag(buf.tag);
    if decoded.ncaps > 0 {
        return Err(WireError::FrameOverflow);
    }

    let len = decoded.len.min(buf.data.len());
    if len > bytes.len() {
        return Err(WireError::Truncated);
    }

    let _ = handles;
    bytes[..len].copy_from_slice(&buf.data[..len]);
    Ok(MessageLen::new(len, 0))
}
