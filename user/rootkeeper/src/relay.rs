//! Relay логирования rootkeeper: сток - bootstrap-капа ядра, которая процесс не
//! покидает, а подсистемам достаются только лог-сессии.

use core::num::NonZeroU64;

use log_relay::{KernelLogSink, LogRelayThread};
use runtime::{OwnedHandle, Result, Timeout};
use spin::Mutex;
use syscall::Handle;

/// Значок лог-сессии супервизора.
pub const SUPERVISOR_BADGE: NonZeroU64 = NonZeroU64::new(1).expect("badge is non-zero");

/// Значок лог-сессии файловой подсистемы.
pub const FILESYSTEM_BADGE: NonZeroU64 = NonZeroU64::new(2).expect("badge is non-zero");

// Цикл пересылки процесса. Владение общее, а не на стеке вызывающего: путь
// паники обязан остановить цикл, иначе живой поток relay не даёт процессу уйти.
static RELAY: Mutex<Option<LogRelayThread>> = Mutex::new(None);

/// Заводит лог-порт и его выделенный поток. `kernel` - bootstrap-капа ядра:
/// её видит только сток relay.
pub fn start(kernel: Handle) -> Result<()> {
    let relay = LogRelayThread::start(source_name, KernelLogSink::new(kernel))?;
    *RELAY.lock() = Some(relay);
    Ok(())
}

/// Минтит лог-сессию со значком `badge`.
#[must_use]
pub fn session(badge: NonZeroU64) -> Option<OwnedHandle> {
    RELAY.lock().as_ref()?.session(badge).ok()
}

/// Минтит лог-сессию подсистемы, раздающей логирование своим процессам.
#[must_use]
pub fn delegated_session(badge: NonZeroU64) -> Option<OwnedHandle> {
    RELAY.lock().as_ref()?.delegated_session(badge).ok()
}

/// Waitable handle потока пересылки.
#[must_use]
pub fn thread_handle() -> Option<Handle> {
    RELAY.lock().as_ref().map(|relay| relay.handle().as_raw())
}

/// Финальный exit-код завершённого потока пересылки.
#[must_use]
pub fn exit_code() -> Option<u32> {
    RELAY.lock().as_ref()?.exit_code().ok()
}

/// Закрывает лог-порт и ждёт завершения цикла пересылки.
///
/// Занятый лок оставляет цикл живым: ожидание лока на пути завершения
/// превращает отказ в зависание.
pub fn shutdown(timeout: Timeout) {
    if let Some(mut held) = RELAY.try_lock()
        && let Some(relay) = held.take()
    {
        let _ = relay.shutdown(timeout);
    }
}

fn source_name(badge: u64) -> Option<&'static str> {
    match badge {
        _ if badge == SUPERVISOR_BADGE.get() => Some("rootkeeper"),
        _ if badge == FILESYSTEM_BADGE.get() => Some("filesystem"),
        _ => None,
    }
}
