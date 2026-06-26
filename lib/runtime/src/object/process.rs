//! Типизированная обёртка над процессом ядра.

use alloc::vec::Vec;

use syscall::{SIGNALED, Timeout};

use crate::{
    error::{Error, Result, unit, value},
    handle::{BorrowedHandle, OwnedHandle},
    object::{Thread, ThreadEntry},
    svc,
};

/// Владеет хэндлом `Process` и закрывает его на drop.
#[derive(Debug)]
pub struct Process {
    handle: OwnedHandle,
}

impl Process {
    /// Создаёт пустой процесс с именем `name`.
    pub fn create(name: &str) -> Result<Self> {
        svc::process_create(name.as_bytes())
            // SAFETY: handle только что создан syscall'ом, мы единственный владелец.
            .map(|handle| Self::from_handle(unsafe { OwnedHandle::from_handle(handle) }))
            .map_err(Error::Syscall)
    }

    /// Возвращает обёртку над собственным процессом.
    pub fn self_process() -> Result<Self> {
        svc::process_self()
            // SAFETY: handle только что создан syscall'ом, мы единственный владелец.
            .map(|handle| Self::from_handle(unsafe { OwnedHandle::from_handle(handle) }))
            .map_err(Error::Syscall)
    }

    /// Берёт во владение хэндл `Process`.
    pub fn from_handle(handle: OwnedHandle) -> Self {
        Self { handle }
    }

    /// Заимствование хэндла на время одного вызова.
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.handle.borrow()
    }

    /// Загружает образ в процесс из сериализованного дескриптора `desc`.
    pub fn load_image(&self, desc: &[u8]) -> Result<()> {
        unit(svc::process_load_image(self.handle.as_raw(), desc))
    }

    /// Стартует первый поток процесса с параметрами `entry`, передавая
    /// bootstrap-хэндлы `handles` потомку. На `Ok` ядро забрало хэндлы (ушли
    /// потомку), на `Err` они закрываются своим `Drop`.
    pub fn start(&self, entry: ThreadEntry, handles: Vec<OwnedHandle>) -> Result<Thread> {
        let ids: Vec<u32> = handles.iter().map(|handle| handle.as_raw().raw()).collect();
        let priority_and_count = u64::from(entry.priority) | ((handles.len() as u64) << 32);

        let result = svc::process_start(
            self.handle.as_raw(),
            entry.entry_pc,
            entry.user_sp,
            entry.arg,
            priority_and_count,
            ids.as_ptr() as u64,
        );

        match result {
            Ok(handle) => {
                for handle in handles {
                    let _ = handle.into_raw();
                }
                // SAFETY: handle только что создан syscall'ом, мы единственный владелец.
                Ok(Thread::from_handle(unsafe {
                    OwnedHandle::from_handle(handle)
                }))
            }
            Err(e) => Err(Error::Syscall(e)),
        }
    }

    /// Блокирует до терминации процесса (`SIGNALED`) либо тайм-аута.
    pub fn join(&self, timeout: Timeout) -> Result<()> {
        value(svc::signal_wait_one(
            self.handle.as_raw(),
            SIGNALED,
            timeout.raw(),
        ))
        .map(|_| ())
    }

    /// Финальный exit-код завершённого процесса.
    pub fn exit_code(&self) -> Result<u32> {
        value(svc::process_exit_code(self.handle.as_raw())).map(|code| {
            #[allow(clippy::cast_possible_truncation)]
            let code = code as u32;
            code
        })
    }

    /// Завершает процесс с кодом `exit_code`.
    pub fn terminate(&self, exit_code: u32) -> Result<()> {
        unit(svc::process_terminate(
            self.handle.as_raw(),
            u64::from(exit_code),
        ))
    }

    /// Отдаёт владеемый хэндл.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }
}
