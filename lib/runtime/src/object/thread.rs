//! Типизированная обёртка над потоком ядра и параметры его запуска.

use syscall::{SIGNALED, Timeout};

use crate::{
    error::{Error, Result, unit, value},
    handle::{BorrowedHandle, OwnedHandle},
    svc,
};

/// Приоритет потока: занимает биты [0..8) ABI, биты [8..32) нулевые по
/// построению, поэтому отдельная проверка `priority > 0xFF` не нужна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Priority(u8);

impl Priority {
    /// Оборачивает значение приоритета.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }
}

impl From<Priority> for u64 {
    fn from(priority: Priority) -> Self {
        Self::from(priority.0)
    }
}

/// Параметры запуска потока: точка входа, вершина стека, аргумент в X0 первой
/// инструкции и приоритет.
#[derive(Debug, Clone, Copy)]
pub struct ThreadEntry {
    /// Адрес первой инструкции потока.
    pub entry_pc: u64,
    /// Вершина user-стека потока.
    pub user_sp: u64,
    /// Значение первой инструкции (X0) для `Thread::create`
    pub arg: u64,
    /// Приоритет потока.
    pub priority: Priority,
}

/// Владеет хэндлом `Thread` и закрывает его на drop.
#[derive(Debug)]
pub struct Thread {
    handle: OwnedHandle,
}

impl Thread {
    /// Создаёт поток в процессе `process` с параметрами `entry`.
    pub fn create(process: BorrowedHandle<'_>, entry: ThreadEntry) -> Result<Self> {
        svc::thread_create(
            process.as_raw(),
            entry.entry_pc,
            entry.user_sp,
            entry.arg,
            u64::from(entry.priority),
        )
        .map(|handle| Self::from_handle(OwnedHandle::from_handle(handle)))
        .map_err(Error::Syscall)
    }

    /// Возвращает обёртку над собственным потоком.
    pub fn self_thread() -> Result<Self> {
        svc::thread_self()
            .map(|handle| Self::from_handle(OwnedHandle::from_handle(handle)))
            .map_err(Error::Syscall)
    }

    /// Берёт во владение хэндл `Thread`.
    pub fn from_handle(handle: OwnedHandle) -> Self {
        Self { handle }
    }

    /// Заимствование хэндла на время одного вызова.
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.handle.borrow()
    }

    /// Финальный exit-код завершённого потока.
    pub fn exit_code(&self) -> Result<u32> {
        value(svc::thread_exit_code(self.handle.as_raw())).map(|code| (code & 0xFFFF_FFFF) as u32)
    }

    /// Блокирует до терминации потока (`SIGNALED`) либо тайм-аута.
    pub fn join(&self, timeout: Timeout) -> Result<()> {
        super::wait_signals(self.handle.as_raw(), SIGNALED, timeout).map(|_| ())
    }

    /// Завершает поток с кодом `exit_code`.
    pub fn terminate(&self, exit_code: u32) -> Result<()> {
        unit(svc::thread_terminate(
            self.handle.as_raw(),
            u64::from(exit_code),
        ))
    }

    /// Отдаёт владеемый хэндл.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }
}
