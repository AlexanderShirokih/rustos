//! Окружение фасада поверх объектов ядра. Сессия становится транспортом её клиента, а
//! capability региона - отображением данных сессии.

use filesystem_abi::{FileSystemError, file_system, namespace};
use kernel_objects::{Mapping, MemoryRegion, ReceivedHandle, SessionTransport};
use syscall::{UserMemFlags, kind};

use crate::{
    FileSystem,
    environment::{Environment, SharedRegion},
};

/// Окружение фасада файловой подсистемы поверх объектов ядра.
pub struct FileSystemEnvironment;

impl FileSystem<FileSystemEnvironment> {
    /// Фасад над сессией файловой системы. Путь отсчитывается от корня её области.
    pub fn windowed(session: kernel_objects::Session<file_system::Protocol>) -> Self {
        Self::windowed_with_environment(FileSystemEnvironment, session.client().into_channel())
    }

    /// Фасад над маршрутизатором имён. Путь разбирается по монтированиям.
    pub fn routed(namespace: kernel_objects::Session<namespace::Protocol>) -> Self {
        Self::routed_with_environment(FileSystemEnvironment, namespace.client().into_channel())
    }
}

/// Отображённый регион данных сессии вместе с capability самого региона.
pub struct SessionRegion {
    mapping: Mapping,
    _region: MemoryRegion,
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
    type Handle<K: syscall::ObjectKind> = ReceivedHandle<K>;
    type Transport = SessionTransport;
    type Region = SessionRegion;

    fn connect(
        &self,
        session: ipc::wire::Session<file_system::Protocol, ReceivedHandle<kind::Port>>,
    ) -> Result<SessionTransport, FileSystemError> {
        Ok(kernel_objects::Session::from_session(session)
            .client()
            .into_channel())
    }

    fn map(
        &self,
        region: ReceivedHandle<kind::MemoryRegion>,
    ) -> Result<SessionRegion, FileSystemError> {
        // Маппинг жив, пока открыта капа региона.
        let region = MemoryRegion::from(region);
        let mapping = region
            .map_full(UserMemFlags::ReadWrite)
            .map_err(|_| FileSystemError::NoSpace)?;
        Ok(SessionRegion {
            mapping,
            _region: region,
        })
    }
}
