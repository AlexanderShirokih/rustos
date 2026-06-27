//! Типизированная обёртка над сигнальным объектом ядра.

use syscall::{SIGNALED, Timeout, WakeCount};

use crate::{
    error::{Error, Result, unit},
    handle::{BorrowedHandle, OwnedHandle},
    svc,
};

/// Владеет хэндлом `Signal` и закрывает его на drop.
#[derive(Debug)]
pub struct Signal {
    handle: OwnedHandle,
}

impl Signal {
    /// Создаёт пустой `Signal` в текущей таблице.
    pub fn create() -> Result<Self> {
        svc::signal_create()
            // SAFETY: handle только что создан syscall'ом, мы единственный владелец.
            .map(|handle| Self::from_handle(unsafe { OwnedHandle::from_handle(handle) }))
            .map_err(Error::Syscall)
    }

    /// Берёт во владение хэндл `Signal`.
    pub fn from_handle(handle: OwnedHandle) -> Self {
        Self { handle }
    }

    /// Заимствование хэндла на время одного вызова.
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.handle.borrow()
    }

    /// Ждёт пересечения с `mask` до `timeout`; возвращает наблюдаемую маску.
    pub fn wait(&self, mask: u32, timeout: Timeout) -> Result<u32> {
        super::wait_signals(self.handle.as_raw(), mask, timeout)
    }

    /// Выставляет биты `set` и снимает `clear`, будя `count` ждущих.
    pub fn set(&self, set: u32, clear: u32, count: WakeCount) -> Result<()> {
        unit(svc::signal_set(self.handle.as_raw(), set, clear, count))
    }

    /// Поднимает `SIGNALED` и будит всех ждущих.
    pub fn notify_all(&self) -> Result<()> {
        self.set(SIGNALED, 0, WakeCount::All)
    }

    /// Отдаёт владеемый хэндл.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }
}
