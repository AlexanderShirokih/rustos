//! Типизированная обёртка над процессом ядра.

use syscall::{SIGNALED, Timeout, kind};

use crate::{
    error::{Error, Result, unit, value},
    handle::OwnedHandle,
    object::{Resource, Thread, ThreadEntry},
    svc,
};

/// Владеет хэндлом `Process` и закрывает его на drop.
pub type Process = OwnedHandle<kind::Process>;

impl OwnedHandle<kind::Process> {
    /// Создаёт пустой процесс с именем `name`.
    pub fn create(name: &str) -> Result<Self> {
        svc::process_create(name.as_bytes())
            .map(Self::from_handle)
            .map_err(Error::Syscall)
    }

    /// Возвращает обёртку над собственным процессом.
    pub fn self_process() -> Result<Self> {
        svc::process_self()
            .map(Self::from_handle)
            .map_err(Error::Syscall)
    }

    /// Загружает образ в процесс из сериализованного дескриптора `desc`.
    pub fn load_image(&self, desc: &[u8]) -> Result<()> {
        unit(svc::process_load_image(self.as_raw(), desc))
    }

    /// Запускает первый поток процесса с параметрами `entry`, передавая потомку
    /// стартовый хэндл-канал `handle`. `resource` задаёт метеринг-ресурс
    /// потомка, `None` отдаёт ему ресурс вызывающего. На `Ok` ядро забрало
    /// `handle` (ушёл потомку), на `Err` он закрывается своим `Drop`.
    pub fn start<K: syscall::ObjectKind>(
        &self,
        entry: ThreadEntry,
        handle: OwnedHandle<K>,
        resource: Option<&Resource>,
    ) -> Result<Thread> {
        let result = svc::process_start(
            self.as_raw(),
            entry.entry_pc,
            entry.user_sp,
            handle.as_raw(),
            u64::from(entry.priority),
            resource.map(Resource::as_raw),
        );

        match result {
            Ok(thread) => {
                let _ = handle.into_raw();
                Ok(Thread::from_handle(thread))
            }
            Err(e) => Err(Error::Syscall(e)),
        }
    }

    /// Блокирует до терминации процесса либо тайм-аута.
    pub fn join(&self, timeout: Timeout) -> Result<()> {
        super::wait_signals(self.as_raw(), SIGNALED, timeout).map(|_| ())
    }

    /// Финальный exit-код завершённого процесса.
    pub fn exit_code(&self) -> Result<u32> {
        value(svc::process_exit_code(self.as_raw())).map(|code| (code & 0xFFFF_FFFF) as u32)
    }

    /// Завершает процесс с кодом `exit_code`.
    pub fn terminate(&self, exit_code: u32) -> Result<()> {
        unit(svc::process_terminate(self.as_raw(), u64::from(exit_code)))
    }
}
