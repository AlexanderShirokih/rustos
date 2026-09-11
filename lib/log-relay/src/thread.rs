//! Лог-порт и его цикл пересылки в выделенном потоке.

use core::num::NonZeroU64;

use ipc::wire::MESSAGE_INLINE_MAX;
use kernel_objects::{Port, Result, Session};
use log_abi::log;
use runtime::{JoinHandle, Priority, Timeout, spawn_owned};
use syscall::Rights;

use crate::relay::{LogRelay, LogSink, SourceNames};

const RELAY_STACK: u64 = 0x8000;

const RELAY_STACK_FRAME_BUFFERS: usize = 4;

const RELAY_STACK_CALL_RESERVE: usize = 0x1000;

const _: () = assert!(
    RELAY_STACK
        >= (RELAY_STACK_FRAME_BUFFERS * MESSAGE_INLINE_MAX + RELAY_STACK_CALL_RESERVE) as u64
);

/// Лог-порт с циклом пересылки в отдельном потоке. Корневая capability не
/// покидает владельца и держит порт с получателем. Пока она открыта,
/// отправители паркуются и на уже мёртвом цикле. Владелец закрывает её через
/// `shutdown`.
pub struct LogRelayThread {
    root: Port,
    thread: JoinHandle,
}

impl LogRelayThread {
    /// Заводит лог-порт и отдаёт его цикл новому потоку со стоком `sink`.
    /// `names` именует значки подсистем в журнале.
    pub fn start<S: LogSink + Send + 'static, N: SourceNames + 'static>(
        names: N,
        sink: S,
    ) -> Result<Self> {
        let root = Port::create()?;
        let mut relay = LogRelay::new(root.duplicate(Rights::READ, None)?, names, sink);
        let thread = spawn_owned(
            move || {
                relay.serve();
                0
            },
            RELAY_STACK,
            Priority::new(1),
        )?;
        Ok(Self { root, thread })
    }

    /// Лог-сессия со значком `badge` и правами `WRITE | TRANSFER | DUPLICATE`. Права приёма
    /// сессия не даёт.
    pub fn session(&self, badge: NonZeroU64) -> Result<Session<log::Protocol>> {
        self.root
            .duplicate(
                Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE,
                Some(badge),
            )
            .map(Session::from_handle)
    }

    /// Закрывает лог-порт и ждёт завершения цикла.
    pub fn shutdown(self, timeout: Timeout) -> Result<()> {
        let Self { root, thread } = self;
        drop(root);
        Ok(thread.join(timeout)?)
    }
}
