//! Production-цепочка userland: запуск rootkeeper-процесса с bootstrap-каналом
//! и логгер кадров local-конца в klog.

extern crate alloc;

use alloc::{sync::Arc, vec};

use klog::{info, warn};
use kobject::{
    CHANNEL_PEER_CLOSED, CHANNEL_READABLE, Channel, Handle, IpcError, KObject, Rights,
    channel_read, install_handle, object_wait_one,
};
use process::{UserImageFromAbiError, user_image_parts_from_entry};
use scheduler::{Priority, UserProcessLaunch, UserProcessLaunchInfo};
use userland_abi::{
    BootstrapHello, UserlandImage, UserlandImageError, parse_bootstrap_hello, parse_bootstrap_log,
};

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
/// с именем bootstrap-entry и peer-концом bootstrap-канала как initial handle 0.
pub fn spawn_rootkeeper(
    launcher: &dyn UserProcessLauncher,
    blob: &'static [u8],
    user_va_end: usize,
) -> Result<RootkeeperLaunch, RootkeeperSpawnError> {
    let image = UserlandImage::parse(blob).map_err(RootkeeperSpawnError::Image)?;
    let entry = image.bootstrap_entry();
    let name = core::str::from_utf8(entry.name_bytes()).unwrap_or("rootkeeper");
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
        .spawn_user_process_with_launch(name, &user_image, Priority::normal(), 2, launch)
        .map_err(RootkeeperSpawnError::Spawn)?;

    Ok(RootkeeperLaunch {
        channel: local,
        info,
    })
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
                BootstrapFrame::Log(text) => info!("{}", text),
                BootstrapFrame::Unknown { len } => {
                    warn!("bootstrap-log: unknown frame, len={}", len);
                }
            }
        }
    }
}

/// Кадр bootstrap-канала, распознанный по магии.
#[derive(Debug, PartialEq, Eq)]
enum BootstrapFrame<'a> {
    Hello(BootstrapHello),
    Log(&'a str),
    Unknown { len: usize },
}

/// Классифицирует кадр по магии: hello, log (только валидный utf8)
/// либо unknown с длиной кадра.
fn classify_frame(bytes: &[u8]) -> BootstrapFrame<'_> {
    if let Ok(hello) = parse_bootstrap_hello(bytes) {
        return BootstrapFrame::Hello(hello);
    }
    if let Ok(payload) = parse_bootstrap_log(bytes)
        && let Ok(text) = core::str::from_utf8(payload)
    {
        return BootstrapFrame::Log(text);
    }
    BootstrapFrame::Unknown { len: bytes.len() }
}

#[cfg(test)]
mod tests {
    use userland_abi::{
        BOOTSTRAP_ABI_VERSION, BOOTSTRAP_HELLO_MAGIC, BOOTSTRAP_HELLO_SIZE, BOOTSTRAP_LOG_FRAME_MAX,
        BOOTSTRAP_LOG_HEADER_SIZE, encode_bootstrap_log,
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
        let bytes = hello_bytes();
        let frame = classify_frame(&bytes);
        assert!(
            matches!(frame, BootstrapFrame::Hello(hello) if hello.version == BOOTSTRAP_ABI_VERSION)
        );
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
    fn classifies_log() {
        let mut frame = [0u8; BOOTSTRAP_LOG_FRAME_MAX];
        let len = encode_bootstrap_log(b"[TEST-PASS: smoke]", &mut frame).expect("log encodes");

        assert_eq!(
            classify_frame(&frame[..len]),
            BootstrapFrame::Log("[TEST-PASS: smoke]")
        );
    }

    #[test]
    fn classifies_empty_log() {
        let mut frame = [0u8; BOOTSTRAP_LOG_FRAME_MAX];
        let len = encode_bootstrap_log(b"", &mut frame).expect("log encodes");

        assert_eq!(classify_frame(&frame[..len]), BootstrapFrame::Log(""));
    }

    #[test]
    fn classifies_invalid_utf8_log_as_unknown() {
        let mut frame = [0u8; BOOTSTRAP_LOG_FRAME_MAX];
        let len = encode_bootstrap_log(&[0xFF, 0xFE], &mut frame).expect("log encodes");

        assert_eq!(
            classify_frame(&frame[..len]),
            BootstrapFrame::Unknown {
                len: BOOTSTRAP_LOG_HEADER_SIZE + 2
            }
        );
    }

    #[test]
    fn classifies_hello_prefix_as_unknown() {
        let bytes = hello_bytes();
        assert_eq!(
            classify_frame(&bytes[..BOOTSTRAP_HELLO_SIZE - 1]),
            BootstrapFrame::Unknown {
                len: BOOTSTRAP_HELLO_SIZE - 1
            }
        );
    }
}
