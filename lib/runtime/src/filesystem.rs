//! Окружение фасада файловой подсистемы. Capability становится портом сессии и отображённым
//! регионом её данных.

use filesystem_abi::FileSystemError;
use filesystem_client::{Environment, SharedRegion};
use ipc::{
    MessageLen, Transport,
    wire::{Cap, IpcError},
};
use syscall::{Timeout, UserMemFlags};

use crate::{Mapping, MemoryRegion, OwnedHandle, PortTransport};

/// Окружение фасада файловой подсистемы поверх объектов рантайма.
pub struct FileSystemEnvironment;

/// Порт сессии файловой системы, владеющий своим хэндлом.
pub struct SessionPort {
    transport: PortTransport,
    _handle: OwnedHandle,
}

/// Отображённый регион данных сессии, владеющий своим региональным хэндлом.
pub struct SessionRegion {
    mapping: Mapping,
    _region: MemoryRegion,
}

impl SessionPort {
    /// Порт поверх хэндла сессии.
    pub fn over(handle: OwnedHandle) -> Self {
        Self {
            transport: PortTransport::client(&handle),
            _handle: handle,
        }
    }
}

impl Transport for SessionPort {
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
    type Transport = SessionPort;
    type Region = SessionRegion;

    fn connect(&self, port: Cap) -> Result<SessionPort, FileSystemError> {
        Ok(SessionPort::over(OwnedHandle::adopt(port)))
    }

    fn map(&self, region: Cap) -> Result<SessionRegion, FileSystemError> {
        // Маппинг жив, пока открыта капа региона.
        let region = MemoryRegion::adopt(region);
        let mapping = region
            .map_full(UserMemFlags::ReadWrite)
            .map_err(|_| FileSystemError::NoSpace)?;
        Ok(SessionRegion {
            mapping,
            _region: region,
        })
    }
}
