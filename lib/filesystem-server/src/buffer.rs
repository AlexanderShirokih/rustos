//! Буфер данных сессии: отображение региона сервера и производная капа клиента.

use alloc::vec::Vec;

use filesystem_abi::FileSystemError;
use kernel_objects::{Mapping, MemoryRegion, Resource};
use runtime::{MemoryAccess, UserMemFlags};
use syscall::Rights;
use userland::USERLAND_PAGE_SIZE;

use crate::error::no_space;

const BUFFER_MAX: u64 = 64 * 1024;

pub(crate) struct DataBuffer {
    mapping: Mapping,
    staging: Vec<u8>,
}

impl DataBuffer {
    pub(crate) fn create(resource: &Resource, size: u32) -> Result<Self, FileSystemError> {
        let bytes = u64::from(size).div_ceil(USERLAND_PAGE_SIZE) * USERLAND_PAGE_SIZE;
        if bytes == 0 || bytes > BUFFER_MAX {
            return Err(FileSystemError::NoSpace);
        }
        let region = resource
            .create_virtual(bytes, MemoryAccess::RW)
            .map_err(no_space)?;
        let mapping = Mapping::new(region, bytes, UserMemFlags::ReadWrite)
            .map_err(|refused| no_space(refused.error))?;
        let len = usize::try_from(bytes).map_err(|_| FileSystemError::NoSpace)?;
        let mut staging = Vec::new();
        staging
            .try_reserve_exact(len)
            .map_err(|_| FileSystemError::NoSpace)?;
        staging.resize(len, 0);
        Ok(Self { mapping, staging })
    }

    pub(crate) fn share(&self) -> Result<MemoryRegion, FileSystemError> {
        // Права клиента без `DUPLICATE`: производную капу он не переминчивает, и закрытие корневой
        // капы сервером отзывает её.
        self.mapping
            .region()
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
            .map_err(no_space)
    }

    pub(crate) fn size(&self) -> usize {
        self.staging.len()
    }

    pub(crate) fn publish<T>(
        &mut self,
        len: usize,
        fill: impl FnOnce(&mut [u8]) -> Result<T, FileSystemError>,
    ) -> Result<T, FileSystemError> {
        let head = self
            .staging
            .get_mut(..len)
            .ok_or(FileSystemError::BufferTooSmall)?;
        // Байты, до которых не дошёл `fill`, уходят клиенту нулями. Данные
        // предыдущей операции сессии в его буфер не просачиваются.
        head.fill(0);
        let value = fill(head)?;
        self.mapping
            .copy_in(head)
            .map_err(|_| FileSystemError::BufferTooSmall)?;
        Ok(value)
    }

    pub(crate) fn snapshot<T>(
        &mut self,
        len: usize,
        take: impl FnOnce(&[u8]) -> Result<T, FileSystemError>,
    ) -> Result<T, FileSystemError> {
        let head = self
            .staging
            .get_mut(..len)
            .ok_or(FileSystemError::BufferTooSmall)?;
        // Страницы буфера отображены и у клиента, который пишет в них в любой момент. Кадр
        // работает с приватной копией.
        self.mapping
            .copy_out(head)
            .map_err(|_| FileSystemError::BufferTooSmall)?;
        take(head)
    }
}
