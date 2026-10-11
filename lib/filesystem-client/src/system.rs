//! Окружение фасада поверх объектов ядра. Сессия становится транспортом её клиента, а
//! capability региона - отображением данных сессии.

use filesystem_abi::{FileSystemError, file_system, namespace};
use kernel_objects::{IssuedHandle, Mapping, MemoryRegion, ReceivedSession, SessionTransport};
use syscall::{UserMemFlags, kind};

use crate::{
    FileSystem,
    environment::{Environment, SharedRegion},
};

/// Окружение фасада файловой подсистемы поверх объектов ядра.
pub struct FileSystemEnvironment;

impl FileSystem<FileSystemEnvironment> {
    /// Фасад над маршрутизатором имён. Путь разбирается по монтированиям.
    pub fn new(namespace: kernel_objects::Session<namespace::Protocol>) -> Self {
        Self::with_environment(FileSystemEnvironment, namespace.client().into_channel())
    }
}

/// Отображённый регион данных сессии.
pub struct SessionRegion {
    mapping: Mapping,
}

impl SharedRegion for SessionRegion {
    fn size(&self) -> usize {
        usize::try_from(self.mapping.size_bytes()).unwrap_or(usize::MAX)
    }

    fn read_at(&self, offset: usize, dst: &mut [u8]) -> Result<(), FileSystemError> {
        self.mapping
            .copy_out_at(offset, dst)
            .map_err(|_| FileSystemError::BufferTooSmall)
    }

    fn write_at(&self, offset: usize, src: &[u8]) -> Result<(), FileSystemError> {
        self.mapping
            .copy_in_at(offset, src)
            .map_err(|_| FileSystemError::BufferTooSmall)
    }
}

impl Environment for FileSystemEnvironment {
    type Handle<K: syscall::ObjectKind> = IssuedHandle<K>;
    type Transport = SessionTransport;
    type Region = SessionRegion;

    fn connect(
        &self,
        session: ReceivedSession<file_system::Protocol>,
    ) -> Result<SessionTransport, FileSystemError> {
        Ok(kernel_objects::Session::from_session(session)
            .client()
            .into_channel())
    }

    fn map(
        &self,
        region: IssuedHandle<kind::MemoryRegion>,
    ) -> Result<SessionRegion, FileSystemError> {
        let mapping = Mapping::full(MemoryRegion::from(region), UserMemFlags::ReadWrite)
            .map_err(|_| FileSystemError::NoSpace)?;
        Ok(SessionRegion { mapping })
    }
}
