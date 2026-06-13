//! Production-цепочка userland: запуск первого (bootstrap) userland-процесса с bootstrap-каналом.

extern crate alloc;

use alloc::{sync::Arc, vec};

use bootstrap::{BootstrapService, dispatch_bootstrap};
use ipc::{
    MessageLen, Transport,
    wire::{IpcError as WireError, Str},
};
use klog::{info, warn};
use kobject::{
    CHANNEL_PEER_CLOSED, CHANNEL_READABLE, Channel, Handle, HandleId, IpcError as KernelIpcError,
    KObject, Message, Rights, channel_read, channel_write, install_handle, object_wait_one,
};
use process::{UserImageFromModelError, user_image_parts_from_entry};
use scheduler::{Priority, UserProcessLaunch, UserProcessLaunchInfo};
use userland::EntryView;
use userland_image::{ImageDecodeError, decode};

use crate::user_process::{SpawnUserError, UserProcessLauncher};

/// Запущенный bootstrap-процесс: local-конец bootstrap-канала остаётся у ядра,
/// `info` держит process/thread KO для наблюдения за завершением.
pub struct BootstrapLaunch {
    pub channel: Arc<Channel>,
    pub info: UserProcessLaunchInfo,
}

/// Ошибки запуска bootstrap-процесса.
#[derive(Debug)]
pub enum BootstrapSpawnError {
    Image(ImageDecodeError),
    Model(UserImageFromModelError),
    Spawn(SpawnUserError),
}

/// Разбирает `blob` и спавнит процесс с именем bootstrap-entry как initial handle 0.
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

    // local остаётся у ядра для чтения; peer уходит bootstrap-процессу. Запись в peer
    // кладёт сообщение в очередь local, где и поднимется CHANNEL_READABLE.
    let (local, peer) = Channel::create_pair(0);
    let peer_ko = KObject::Channel(peer);
    let peer_handle = Handle::new(peer_ko.clone(), Rights::defaults_for(&peer_ko));
    let launch = UserProcessLaunch::new()
        .initial_handles(vec![peer_handle])
        .bootstrap_handle(0);

    let info = launcher
        .spawn_user_process_with_launch(name, &user_image, Priority::normal(), 2, launch)
        .map_err(BootstrapSpawnError::Spawn)?;

    Ok(BootstrapLaunch {
        channel: local,
        info,
    })
}

/// Цикл логгера bootstrap-канала: сигнальное ожидание READABLE|PEER_CLOSED,
/// дренаж очереди контрактом `Bootstrap`, выход по PEER_CLOSED при дочитанной
/// очереди.
pub fn run_bootstrap_log(local: &Arc<Channel>) {
    let chan_ko = KObject::Channel(local.clone());
    let chan_handle = Handle::new(
        chan_ko,
        Rights::READ | Rights::WRITE | Rights::WAIT | Rights::INSPECT,
    );
    let chan_id = match install_handle(chan_handle) {
        Ok(id) => id,
        Err(e) => {
            warn!("bootstrap-log: install_handle failed: {:?}", e);
            return;
        }
    };

    let transport = ChannelTransport::new(chan_id);
    let mut sink = BootstrapLog;

    loop {
        match transport.wait_readable(u64::MAX) {
            Ok(()) => {}
            Err(WireError::PeerClosed) => {
                info!("bootstrap-log: peer closed, exiting");
                return;
            }
            Err(e) => {
                warn!("bootstrap-log: wait failed: {:?}", e);
                return;
            }
        }

        loop {
            match dispatch_bootstrap(&mut sink, &transport) {
                Ok(()) => {}
                Err(WireError::WouldBlock) => break,
                Err(WireError::PeerClosed) => {
                    info!("bootstrap-log: peer closed, exiting");
                    return;
                }
                Err(e) => {
                    warn!("bootstrap-log: dispatch failed: {:?}", e);
                    break;
                }
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

/// Порт ipc-контракта поверх kernel-канала: оборачивает HandleId эндпоинта.
pub struct ChannelTransport {
    handle: HandleId,
}

impl ChannelTransport {
    /// Связывает транспорт с HandleId канального эндпоинта.
    pub fn new(handle: HandleId) -> Self {
        Self { handle }
    }
}

/// Сводит kernel-ошибку канала к ошибке wire-транспорта.
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
        | KernelIpcError::OutOfHandles => WireError::PeerClosed,
    }
}

impl Transport for ChannelTransport {
    fn write_message(&self, bytes: &[u8], _handles: &[u32]) -> Result<(), WireError> {
        let message = Message::from_bytes(bytes).map_err(map_kernel_error)?;
        channel_write(self.handle, message).map_err(map_kernel_error)
    }

    fn read_message(
        &self,
        bytes: &mut [u8],
        _handles: &mut [u32],
    ) -> Result<MessageLen, WireError> {
        let message = channel_read(self.handle).map_err(map_kernel_error)?;
        let payload = message.bytes();
        if payload.len() > bytes.len() {
            return Err(WireError::Truncated);
        }
        bytes[..payload.len()].copy_from_slice(payload);
        Ok(MessageLen::new(payload.len(), 0))
    }

    fn wait_readable(&self, timeout_ns: u64) -> Result<(), WireError> {
        let timeout = if timeout_ns == u64::MAX {
            None
        } else {
            Some(timeout_ns)
        };
        let observed =
            object_wait_one(self.handle, CHANNEL_READABLE | CHANNEL_PEER_CLOSED, timeout)
                .map_err(map_kernel_error)?;
        if observed & CHANNEL_READABLE != 0 {
            Ok(())
        } else if observed & CHANNEL_PEER_CLOSED != 0 {
            Err(WireError::PeerClosed)
        } else {
            Err(WireError::Timeout)
        }
    }
}
