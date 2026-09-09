//! Окружение фасада поверх объектов ядра: capability становится портом сессии и
//! отображённым регионом её данных.

use filesystem_abi::FileSystemError;
use ipc::{MessageLen, Transport, wire::IpcError};
use kernel_objects::{Mapping, MemoryRegion, Port, PortTransport, ReceivedHandle};
use syscall::{Timeout, UserMemFlags, kind};

use crate::{
    FileSystem,
    environment::{Environment, SharedRegion},
};

/// Окружение фасада файловой подсистемы поверх объектов ядра.
pub struct FileSystemEnvironment;

impl FileSystem<FileSystemEnvironment> {
    /// Фасад над сессией файловой системы. Путь отсчитывается от корня её области.
    pub fn windowed(session: Port) -> Self {
        Self::windowed_with_environment(FileSystemEnvironment, SessionPort::over(session))
    }

    /// Фасад над маршрутизатором имён. Путь разбирается по монтированиям.
    pub fn routed(namespace: Port) -> Self {
        Self::routed_with_environment(FileSystemEnvironment, SessionPort::over(namespace))
    }
}

/// Порт сессии файловой системы, владеющий своим хэндлом.
pub struct SessionPort {
    transport: PortTransport,
    _handle: Port,
}

/// Отображённый регион данных сессии вместе с capability самого региона.
pub struct SessionRegion {
    mapping: Mapping,
    _region: MemoryRegion,
}

impl SessionPort {
    /// Порт поверх хэндла сессии.
    pub fn over(handle: Port) -> Self {
        Self {
            transport: PortTransport::client(&handle),
            _handle: handle,
        }
    }
}

impl Transport for SessionPort {
    type Resources = <PortTransport as Transport>::Resources;

    fn take_resources(&self) -> Self::Resources {
        self.transport.take_resources()
    }

    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        self.transport.write_message(bytes, handles)
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        self.transport.read_message(bytes, handles)
    }

    fn wait_readable(&self, timeout: Timeout) -> Result<(), IpcError> {
        self.transport.wait_readable(timeout)
    }
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
    type Transport = SessionPort;
    type Region = SessionRegion;

    fn connect(&self, port: ReceivedHandle<kind::Port>) -> Result<SessionPort, FileSystemError> {
        Ok(SessionPort::over(port.into()))
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
