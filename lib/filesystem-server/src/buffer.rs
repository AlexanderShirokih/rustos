//! Буфер данных сессии: регион сервера, его маппинг и производная капа клиента.

use alloc::vec::Vec;

use filesystem_abi::FileSystemError;
use runtime::{Mapping, MemoryAccess, MemoryRegion, OwnedHandle, Resource, UserMemFlags};
use syscall::Rights;
use userland::USERLAND_PAGE_SIZE;

use crate::error::no_space;

// Потолок буфера данных на сессию: его страницы списываются с Resource сервера, общего на всё
// дерево процессов.
const BUFFER_MAX: u64 = 64 * 1024;

/// Разделяемый с клиентом буфер: регион сервера вместе с его маппингом и
/// приватной копией содержимого.
pub(crate) struct DataBuffer {
    mapping: Mapping,
    region: MemoryRegion,
    /// Копия содержимого в приватной памяти сервера, размером во весь буфер:
    /// страницы буфера отображены и у клиента, который пишет в них в любой
    /// момент: данные кадра сервер берёт и отдаёт копированием.
    staging: Vec<u8>,
}

impl DataBuffer {
    /// Заводит буфер на `size` байт, округлённых вверх до страницы. Нулевой
    /// размер и размер сверх потолка - `NoSpace`.
    pub(crate) fn create(resource: &Resource, size: u32) -> Result<Self, FileSystemError> {
        let bytes = u64::from(size).div_ceil(USERLAND_PAGE_SIZE) * USERLAND_PAGE_SIZE;
        if bytes == 0 || bytes > BUFFER_MAX {
            return Err(FileSystemError::NoSpace);
        }
        let region = resource
            .create_virtual(bytes, MemoryAccess::RW)
            .map_err(no_space)?;
        let mapping = region
            .map(bytes, UserMemFlags::ReadWrite)
            .map_err(no_space)?;
        let len = usize::try_from(bytes).expect("buffer size fits usize");
        let mut staging = Vec::new();
        staging
            .try_reserve_exact(len)
            .map_err(|_| FileSystemError::NoSpace)?;
        staging.resize(len, 0);
        Ok(Self {
            mapping,
            region,
            staging,
        })
    }

    /// Производный RW-хэндл буфера для клиента: без `DUPLICATE` клиент
    /// не переминтит его, а закрытие корневой капы сервером отзывает и его.
    pub(crate) fn share(&self) -> Result<OwnedHandle, FileSystemError> {
        self.region
            .handle()
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, 0)
            .map_err(no_space)
    }

    /// Размер буфера в байтах.
    pub(crate) fn size(&self) -> usize {
        self.staging.len()
    }

    /// Отдаёт `fill` первые `len` байт приватной копии и публикует их в буфере
    /// клиента. Длина сверх размера буфера - `BufferTooSmall`.
    pub(crate) fn publish<T>(
        &mut self,
        len: usize,
        fill: impl FnOnce(&mut [u8]) -> Result<T, FileSystemError>,
    ) -> Result<T, FileSystemError> {
        let head = self
            .staging
            .get_mut(..len)
            .ok_or(FileSystemError::BufferTooSmall)?;
        // Байты, до которых не дошёл `fill`, уходят клиенту нулями: данные
        // предыдущей операции сессии в его буфер не просачиваются.
        head.fill(0);
        let value = fill(head)?;
        self.mapping
            .copy_in(head)
            .map_err(|_| FileSystemError::BufferTooSmall)?;
        Ok(value)
    }

    /// Снимает первые `len` байт буфера клиента в приватную копию и отдаёт их
    /// `take`. Длина сверх размера буфера - `BufferTooSmall`.
    pub(crate) fn snapshot<T>(
        &mut self,
        len: usize,
        take: impl FnOnce(&[u8]) -> Result<T, FileSystemError>,
    ) -> Result<T, FileSystemError> {
        let head = self
            .staging
            .get_mut(..len)
            .ok_or(FileSystemError::BufferTooSmall)?;
        self.mapping
            .copy_out(head)
            .map_err(|_| FileSystemError::BufferTooSmall)?;
        take(head)
    }
}
