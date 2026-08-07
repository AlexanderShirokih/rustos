//! Лог-порт и его цикл пересылки в выделенном потоке.

use core::num::NonZeroU64;

use ipc::wire::MESSAGE_INLINE_MAX;
use runtime::{
    BorrowedHandle, JoinHandle, OwnedHandle, Port, Priority, Result, Timeout, spawn_owned,
};
use syscall::Rights;

use crate::relay::{LogRelay, LogSink, SourceNames};

const RELAY_STACK: u64 = 0x8000;

const RELAY_STACK_FRAME_BUFFERS: usize = 4;

const RELAY_STACK_CALL_RESERVE: usize = 0x1000;

const _: () = assert!(
    RELAY_STACK as usize
        >= RELAY_STACK_FRAME_BUFFERS * MESSAGE_INLINE_MAX + RELAY_STACK_CALL_RESERVE
);

/// Лог-порт с циклом пересылки в отдельном потоке. Корневая capability не
/// покидает владельца и держит порт с получателем: пока она открыта,
/// отправители паркуются и на уже мёртвом цикле. Владелец закрывает её через
/// `shutdown` либо `wait`.
pub struct LogRelayThread {
    root: OwnedHandle,
    thread: JoinHandle,
}

impl LogRelayThread {
    /// Заводит лог-порт и отдаёт его цикл новому потоку со стоком `sink`.
    /// `names` именует значки подсистем в журнале.
    pub fn start<S: LogSink + Send + 'static>(names: SourceNames, sink: S) -> Result<Self> {
        let root = Port::create()?.into_handle();
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

    /// Лог-сессия со значком `badge`: отправка и перенос кадром. Приёма и минта
    /// производных сессия не даёт.
    pub fn session(&self, badge: NonZeroU64) -> Result<OwnedHandle> {
        self.root
            .duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))
    }

    /// Лог-сессия подсистемы, раздающей логирование своим процессам: к правам
    /// сессии добавлен минт производных. Значок set-once, и производные несут
    /// тот же `badge`.
    pub fn delegated_session(&self, badge: NonZeroU64) -> Result<OwnedHandle> {
        self.root.duplicate(
            Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE,
            Some(badge),
        )
    }

    /// Заимствует waitable handle потока relay для supervisor loop.
    #[must_use]
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.thread.handle()
    }

    /// Возвращает финальный exit-код завершённого потока relay.
    pub fn exit_code(&self) -> Result<u32> {
        self.thread.exit_code()
    }

    /// Ждёт завершения цикла пересылки и закрывает лог-порт. Пока цикл жив, не
    /// возвращается.
    pub fn wait(self, timeout: Timeout) -> Result<()> {
        let Self { root, thread } = self;
        let waited = thread.join(timeout);
        drop(root);
        waited
    }

    /// Закрывает лог-порт и ждёт завершения цикла.
    pub fn shutdown(self, timeout: Timeout) -> Result<()> {
        let Self { root, thread } = self;
        drop(root);
        thread.join(timeout)
    }
}
