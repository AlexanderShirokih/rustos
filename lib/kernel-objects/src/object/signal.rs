//! Типизированная обёртка над сигнальным объектом ядра.

use syscall::{SIGNALED, Timeout, WakeCount, kind};

use crate::{
    error::{Error, Result, unit},
    handle::OwnedHandle,
    svc,
};

/// Владеет хэндлом `Signal` и закрывает его на drop.
pub type Signal = OwnedHandle<kind::Signal>;

impl OwnedHandle<kind::Signal> {
    /// Создаёт пустой `Signal` в текущем процессе.
    pub fn create() -> Result<Self> {
        svc::signal_create().map(Self::from).map_err(Error::Syscall)
    }

    /// Ждёт пересечения с `mask` до `timeout`. Возвращает наблюдаемую маску.
    pub fn wait(&self, mask: u32, timeout: Timeout) -> Result<u32> {
        super::wait_signals(self.as_raw(), mask, timeout)
    }

    /// Выставляет биты `set` и снимает `clear`, будя `count` ждущих.
    pub fn set(&self, set: u32, clear: u32, count: WakeCount) -> Result<()> {
        unit(svc::signal_set(self.as_raw(), set, clear, count))
    }

    /// Поднимает `SIGNALED` и будит всех ждущих.
    pub fn notify_all(&self) -> Result<()> {
        self.set(SIGNALED, 0, WakeCount::All)
    }
}
