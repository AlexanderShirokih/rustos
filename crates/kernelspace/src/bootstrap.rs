//! Production-цепочка userland: запуск rootkeeper-процесса с bootstrap-каналом
//! и kernel-таск "bootstrap-log", печатающий кадры local-конца в klog.

extern crate alloc;

use alloc::{sync::Arc, vec};

use klog::{info, warn};
use kobject::{
    CHANNEL_PEER_CLOSED, CHANNEL_READABLE, Channel, Handle, IpcError, KObject, Rights,
    channel_read, install_handle, object_wait_one,
};
use scheduler::{
    Priority, SchedulerService, SchedulerServiceExt, SpawnConfig, UserProcessLaunch,
    UserProcessLaunchInfo,
};
use userland_abi::{
    BootstrapHeartbeat, BootstrapHello, UserlandImage, UserlandImageError,
    parse_bootstrap_heartbeat, parse_bootstrap_hello,
};
use userspace::{UserImageFromAbiError, user_image_parts_from_entry};

use crate::user_process::{SpawnUserError, UserProcessLauncher};

/// Запущенный rootkeeper: local-конец bootstrap-канала остаётся у ядра,
/// `info` держит process/thread KO для наблюдения за завершением.
pub struct RootkeeperLaunch {
    pub channel: Arc<Channel>,
    pub info: UserProcessLaunchInfo,
}

/// Ошибки запуска rootkeeper: разбор образа, мост в `UserImage`, спавн процесса.
#[derive(Debug)]
pub enum RootkeeperSpawnError {
    Image(UserlandImageError),
    Bridge(UserImageFromAbiError),
    Spawn(SpawnUserError),
}

/// Разбирает `blob`, мостит bootstrap-entry в `UserImage` и спавнит процесс
/// "rootkeeper" с peer-концом bootstrap-канала как initial handle 0.
pub fn spawn_rootkeeper(
    launcher: &dyn UserProcessLauncher,
    blob: &[u8],
    user_va_end: usize,
) -> Result<RootkeeperLaunch, RootkeeperSpawnError> {
    let image = UserlandImage::parse(blob).map_err(RootkeeperSpawnError::Image)?;
    let entry = image.bootstrap_entry();
    let parts =
        user_image_parts_from_entry(&entry, user_va_end).map_err(RootkeeperSpawnError::Bridge)?;
    let user_image = parts.image();

    // local остаётся у ядра для чтения; peer уходит rootkeeper'у. Запись в peer
    // кладёт сообщение в очередь local, где и поднимется CHANNEL_READABLE.
    let (local, peer) = Channel::create_pair(0);
    let peer_ko = KObject::Channel(peer);
    let peer_handle = Handle::new(peer_ko.clone(), Rights::defaults_for(&peer_ko));
    let launch = UserProcessLaunch::new()
        .initial_handles(vec![peer_handle])
        .bootstrap_handle(0);

    let info = launcher
        .spawn_user_process_with_launch("rootkeeper", &user_image, Priority::normal(), 2, launch)
        .map_err(RootkeeperSpawnError::Spawn)?;

    Ok(RootkeeperLaunch {
        channel: local,
        info,
    })
}

/// Спавнит kernel-таск "bootstrap-log": кадры `local`-конца уходят в klog,
/// таск завершается по закрытию peer-конца.
pub fn spawn_bootstrap_log(
    scheduler: &Arc<dyn SchedulerService>,
    local: Arc<Channel>,
) -> Result<(), &'static str> {
    scheduler
        .spawn(
            SpawnConfig::new("bootstrap-log").priority(Priority::normal()),
            move || run_bootstrap_log(&local),
        )
        .map_err(|_| "spawn bootstrap-log failed")
        .map(|_| ())
}

/// Цикл логгера bootstrap-канала: сигнальное ожидание READABLE|PEER_CLOSED,
/// дренаж очереди до `ShouldWait`, выход по PEER_CLOSED при дочитанной очереди.
pub fn run_bootstrap_log(local: &Arc<Channel>) {
    let chan_ko = KObject::Channel(local.clone());
    let chan_handle = Handle::new(chan_ko, Rights::READ | Rights::WAIT | Rights::INSPECT);
    let chan_id = match install_handle(chan_handle) {
        Ok(id) => id,
        Err(e) => {
            warn!("bootstrap-log: install_handle failed: {:?}", e);
            return;
        }
    };

    loop {
        match object_wait_one(chan_id, CHANNEL_READABLE | CHANNEL_PEER_CLOSED, None) {
            Ok(observed)
                if observed & CHANNEL_PEER_CLOSED != 0
                    && local.peek_signals() & CHANNEL_READABLE == 0 =>
            {
                info!("bootstrap-log: peer closed, exiting");
                return;
            }
            Ok(_) => {}
            Err(e) => {
                warn!("bootstrap-log: wait failed: {:?}", e);
                return;
            }
        }

        loop {
            let message = match channel_read(chan_id) {
                Ok(message) => message,
                Err(IpcError::ShouldWait) => break,
                Err(IpcError::PeerClosed) => {
                    info!("bootstrap-log: peer closed, exiting");
                    return;
                }
                Err(e) => {
                    warn!("bootstrap-log: read failed: {:?}", e);
                    break;
                }
            };

            match classify_frame(message.bytes()) {
                BootstrapFrame::Hello(hello) => info!("rootkeeper: hello v{}", hello.version),
                BootstrapFrame::Heartbeat(heartbeat) => {
                    info!("rootkeeper: heartbeat #{}", heartbeat.seq);
                }
                BootstrapFrame::Unknown { len } => {
                    warn!("bootstrap-log: unknown frame, len={}", len);
                }
            }
        }
    }
}

/// Кадр bootstrap-канала, распознанный по магии.
#[derive(Debug, PartialEq, Eq)]
enum BootstrapFrame {
    Hello(BootstrapHello),
    Heartbeat(BootstrapHeartbeat),
    Unknown { len: usize },
}

/// Классифицирует кадр по магии: hello, heartbeat либо unknown с длиной кадра.
fn classify_frame(bytes: &[u8]) -> BootstrapFrame {
    if let Ok(hello) = parse_bootstrap_hello(bytes) {
        return BootstrapFrame::Hello(hello);
    }
    if let Ok(heartbeat) = parse_bootstrap_heartbeat(bytes) {
        return BootstrapFrame::Heartbeat(heartbeat);
    }
    BootstrapFrame::Unknown { len: bytes.len() }
}

#[cfg(test)]
mod tests {
    use userland_abi::{
        BOOTSTRAP_ABI_VERSION, BOOTSTRAP_HEARTBEAT_SIZE, BOOTSTRAP_HELLO_MAGIC,
        BOOTSTRAP_HELLO_SIZE, encode_bootstrap_heartbeat,
    };

    use super::*;

    fn hello_bytes() -> [u8; BOOTSTRAP_HELLO_SIZE] {
        let mut bytes = [0u8; BOOTSTRAP_HELLO_SIZE];
        bytes[0..8].copy_from_slice(&BOOTSTRAP_HELLO_MAGIC);
        bytes[8..10].copy_from_slice(&BOOTSTRAP_ABI_VERSION.to_le_bytes());
        bytes
    }

    #[test]
    fn classifies_hello() {
        let frame = classify_frame(&hello_bytes());
        assert!(
            matches!(frame, BootstrapFrame::Hello(hello) if hello.version == BOOTSTRAP_ABI_VERSION)
        );
    }

    #[test]
    fn classifies_heartbeat() {
        let frame = classify_frame(&encode_bootstrap_heartbeat(42));
        assert!(matches!(frame, BootstrapFrame::Heartbeat(hb) if hb.seq == 42));
    }

    #[test]
    fn classifies_hello_sized_garbage_as_unknown() {
        let bytes = [0xA5u8; BOOTSTRAP_HELLO_SIZE];
        assert_eq!(
            classify_frame(&bytes),
            BootstrapFrame::Unknown {
                len: BOOTSTRAP_HELLO_SIZE
            }
        );
    }

    #[test]
    fn classifies_heartbeat_sized_garbage_as_unknown() {
        let bytes = [0xA5u8; BOOTSTRAP_HEARTBEAT_SIZE];
        assert_eq!(
            classify_frame(&bytes),
            BootstrapFrame::Unknown {
                len: BOOTSTRAP_HEARTBEAT_SIZE
            }
        );
    }

    #[test]
    fn classifies_heartbeat_prefix_as_unknown() {
        let bytes = encode_bootstrap_heartbeat(42);
        assert_eq!(
            classify_frame(&bytes[..BOOTSTRAP_HELLO_SIZE]),
            BootstrapFrame::Unknown {
                len: BOOTSTRAP_HELLO_SIZE
            }
        );
    }
}
