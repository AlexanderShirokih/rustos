//! Production-цепочка userland: запуск первого userland-процесса с bootstrap-port'ом.

extern crate alloc;

use alloc::{sync::Arc, vec};
use core::num::NonZeroUsize;

use bootstrap::{BootstrapService, dispatch_bootstrap};
use capability::{
    Capability, CapabilityTarget, IpcError as KernelIpcError, KernelIpcBuffer, Port, Resource,
    ThreadTransport, default_rights_for, port_recv, runtime,
};
use collections::{LockCell, MutexCell};
use ipc::{
    MessageLen, Transport,
    wire::{IpcError as WireError, Str},
};
use klog::{info, warn};
use memory::{AccessMask, physical_address::PageAlignedAddress};
use process::{UserImageFromModelError, user_image_parts_from_entry};
use scheduler::{Priority, UserProcessLaunch, UserProcessLaunchInfo};
use syscall::{IpcBuffer, decode_tag};
use userland::EntryView;
use userland_image::{ImageDecodeError, decode};

use crate::user_process::{SpawnUserError, UserProcessLauncher};

/// Запущенный bootstrap-процесс.
pub struct BootstrapLaunch {
    pub port: Arc<Port>,
    pub info: UserProcessLaunchInfo,
}

/// Ошибки запуска bootstrap-процесса.
#[derive(Debug)]
pub enum BootstrapSpawnError {
    Image(ImageDecodeError),
    Model(UserImageFromModelError),
    Spawn(SpawnUserError),
}

/// Разбирает `blob` и спавнит процесс с именем bootstrap-entry.
pub fn spawn_process(
    launcher: &dyn UserProcessLauncher,
    blob: &'static [u8],
    user_va_end: usize,
) -> Result<BootstrapLaunch, BootstrapSpawnError> {
    let image = decode(blob).map_err(BootstrapSpawnError::Image)?;
    let entry = image.bootstrap_entry();
    let name = entry.name();
    let parts =
        user_image_parts_from_entry(&entry, user_va_end).map_err(BootstrapSpawnError::Model)?;
    let user_image = parts.image();

    // Один Port: ядро держит Arc как получатель; ОДИН handle на тот же
    // объект уходит bootstrap-процессу как initial handle[0] - он клиент.
    let port = Port::new();
    let peer_target = CapabilityTarget::Port(port.clone());
    let peer_handle = Capability::new(peer_target.clone(), default_rights_for(&peer_target));

    // Корневой Resource: полномочие на минтинг физпамяти + носитель
    // ресурсного бюджета. Выдаётся bootstrap-процессу как initial handle[1]
    // (с правами по умолчанию: DUPLICATE|TRANSFER|READ|WRITE), чтобы
    // полномочие перестало быть «спящим» — bootstrap может его делегировать.
    //
    // PA-диапазон/бюджет заданы консервативно: вся 64-бит PA-плоскость, кроме
    // одной хвостовой страницы (во избежание переполнения в `permits`), и
    // 1<<20 страниц = 4 GiB бюджета.
    let root_resource = Resource::new(
        PageAlignedAddress::ZERO,
        NonZeroUsize::new(usize::MAX & !0xFFF).expect("non-zero resource span"),
        AccessMask::RW,
        1 << 20,
    );
    let resource_target = CapabilityTarget::Resource(root_resource.clone());
    let resource_handle = Capability::new(
        resource_target.clone(),
        default_rights_for(&resource_target),
    );

    let launch = UserProcessLaunch::new()
        .initial_handles(vec![peer_handle, resource_handle])
        .bootstrap_handle(0);

    let info = launcher
        .spawn_user_process_with_launch(name, &user_image, Priority::normal(), 2, launch)
        .map_err(BootstrapSpawnError::Spawn)?;

    // Метеринг-ресурс bootstrap-процесса = корневой Resource. Засев до старта
    // scheduler-а, поэтому процесс не успевает аллоцировать раньше.
    info.process_object.set_metering_resource(root_resource);

    Ok(BootstrapLaunch { port, info })
}

/// Цикл логгера bootstrap-port'а: kernel-поток-логгер аллоцирует
/// kernel-резидентный IPC-буфер, строит kernel-транспорт и в бесконечном цикле декодирует кадры контрактом
/// `Bootstrap`, зеркаля их в klog.
///
/// Поскольку логгер держит `Arc<Port>`, ветка `PeerClosed` недостижима.
/// Цикл завершается только по ошибке `dispatch_bootstrap`; машину гасит
/// init по завершению bootstrap-процесса (см. [`crate::init`]).
pub fn run_bootstrap_log(port: &Arc<Port>) {
    let Some(table) = runtime().current_handle_table() else {
        warn!("bootstrap-log: no kernel handle-table; logger not started");
        return;
    };

    let buffer: KernelIpcBuffer = Arc::new(MutexCell::new(IpcBuffer::zeroed()));
    let transport = KernelPortTransport::new(port.clone(), buffer, table);
    let mut sink = BootstrapLog;

    loop {
        match dispatch_bootstrap(&mut sink, &transport) {
            Ok(()) => {}
            Err(WireError::PeerClosed) => {
                info!("bootstrap-log: port closed, exiting");
                return;
            }
            Err(e) => {
                warn!("bootstrap-log: dispatch failed: {:?}", e);
                return;
            }
        }
    }
}

/// Серверная сторона контракта `Bootstrap`: зеркалит строки лога в klog.
struct BootstrapLog;

impl BootstrapService for BootstrapLog {
    fn log(&mut self, message: Str<{ bootstrap::LOG_MESSAGE_MAX }>) {
        info!("{}", message.as_str());
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
    table: Arc<MutexCell<capability::HandleTable>>,
}

impl KernelPortTransport {
    fn new(
        port: Arc<Port>,
        buffer: KernelIpcBuffer,
        table: Arc<MutexCell<capability::HandleTable>>,
    ) -> Self {
        Self {
            port,
            buffer,
            table,
        }
    }

    fn thread_transport(&self) -> ThreadTransport {
        ThreadTransport::new_kernel(self.buffer.clone(), self.table.clone())
    }
}

impl Transport for KernelPortTransport {
    fn write_message(&self, _bytes: &[u8], _handles: &[u32]) -> Result<(), WireError> {
        // Bootstrap - только #[cast]: reply-путь недостижим.
        Err(WireError::PeerClosed)
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, WireError> {
        let receiver = self.thread_transport();
        // Блокирующий приём; сообщение ложится в kernel-буфер.
        port_recv(&self.port, receiver, runtime(), None).map_err(map_kernel_error)?;
        self.buffer
            .with_lock(|buf| load_kernel_buffer(buf, bytes, handles))
    }

    fn wait_readable(&self, _timeout_ns: u64) -> Result<(), WireError> {
        // recv сам блокирует в read_message.
        Ok(())
    }
}

/// Декодирует сообщение из kernel-буфера в `bytes`/`handles` по tag.
fn load_kernel_buffer(
    buf: &IpcBuffer,
    bytes: &mut [u8],
    handles: &mut [u32],
) -> Result<MessageLen, WireError> {
    let (len, ncaps) = decode_tag(buf.tag);
    if ncaps > 0 {
        return Err(WireError::FrameOverflow);
    }
    let len = len.min(buf.data.len());
    if len > bytes.len() {
        return Err(WireError::Truncated);
    }
    let _ = handles;
    bytes[..len].copy_from_slice(&buf.data[..len]);
    Ok(MessageLen::new(len, 0))
}
