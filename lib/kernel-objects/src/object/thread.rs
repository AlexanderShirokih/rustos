//! Типизированная обёртка над потоком ядра и параметры его запуска.

use syscall::{Priority, SIGNALED, Timeout, kind};

use crate::{
    error::{Error, Result, unit, value},
    handle::{BorrowedHandle, OwnedHandle},
    svc,
};

/// Параметры запуска потока задают точку входа, вершину стека, аргумент в X0 первой
/// инструкции и приоритет.
#[derive(Debug, Clone, Copy)]
pub struct ThreadEntry {
    /// Адрес первой инструкции потока.
    pub entry_pc: u64,
    /// Вершина user-стека потока.
    pub user_sp: u64,
    /// Аргумент в X0 первой инструкции потока.
    pub arg: u64,
    /// Приоритет потока.
    pub priority: Priority,
}

/// Владеет хэндлом `Thread` и закрывает его на drop.
pub type Thread = OwnedHandle<kind::Thread>;

impl OwnedHandle<kind::Thread> {
    /// Создаёт поток в процессе `process` с параметрами `entry`.
    pub fn create(process: BorrowedHandle<'_, kind::Process>, entry: ThreadEntry) -> Result<Self> {
        svc::thread_create(
            process.as_raw(),
            entry.entry_pc,
            entry.user_sp,
            entry.arg,
            u64::from(entry.priority),
        )
        .map(Self::from)
        .map_err(Error::Syscall)
    }

    /// Возвращает обёртку над собственным потоком.
    pub fn self_thread() -> Result<Self> {
        svc::thread_self().map(Self::from).map_err(Error::Syscall)
    }

    /// Финальный exit-код завершённого потока.
    pub fn exit_code(&self) -> Result<u32> {
        value(svc::thread_exit_code(self.as_raw())).map(|code| (code & 0xFFFF_FFFF) as u32)
    }

    /// Блокирует до терминации потока (`SIGNALED`) либо тайм-аута.
    pub fn join(&self, timeout: Timeout) -> Result<()> {
        super::wait_signals(self.as_raw(), SIGNALED, timeout).map(|_| ())
    }

    /// Завершает поток с кодом `exit_code`.
    pub fn terminate(&self, exit_code: u32) -> Result<()> {
        unit(svc::thread_terminate(self.as_raw(), u64::from(exit_code)))
    }
}
