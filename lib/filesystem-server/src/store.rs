//! Таблица регионов, на которых лежат байты файловых бекендов.

use alloc::vec::Vec;

use filesystem::{ByteStore, StoreId, Window, insert_store};
use filesystem_abi::{AccessMode, FileSystemError};
use kernel_objects::{MappedPages, Mapping, MemoryRegion, ReadOnlyMapping, RegionKind, Resource};
use runtime::{MemoryAccess, UserMemFlags};
use syscall::{Rights, SyscallError};
use userland::USERLAND_PAGE_SIZE;

use crate::{error::no_space, service::access_for};

enum Store {
    Initial(&'static ReadOnlyMapping),
    Mutable(MemoryRegion),
}

struct Stored {
    generation: u64,
    bytes: Store,
}

pub(crate) struct StoreTable {
    stores: Vec<Option<Stored>>,
    generation: u64,
    resource: Resource,
}

impl StoreTable {
    pub(crate) fn new(resource: Resource, initial: Option<&'static ReadOnlyMapping>) -> Self {
        let mut stores = Vec::new();
        // Регион точки монтирования ложится первым. Окна бекенда адресуют его как `StoreId::MOUNT`.
        if let Some(image) = initial {
            stores.push(Some(Stored {
                generation: 1,
                bytes: Store::Initial(image),
            }));
        }

        let generation = u64::from(!stores.is_empty());
        Self {
            stores,
            generation,
            resource,
        }
    }

    pub(crate) fn resource(&self) -> &Resource {
        &self.resource
    }

    pub(crate) fn import(
        &mut self,
        region: MemoryRegion,
    ) -> Result<(StoreId, u64), FileSystemError> {
        let info = region.inspect().map_err(no_space)?;
        if info.kind != RegionKind::Virtual
            || info.size_bytes == 0
            || !info.size_bytes.is_multiple_of(USERLAND_PAGE_SIZE)
            || info.access.raw() & MemoryAccess::RW.raw() != MemoryAccess::RW.raw()
        {
            return Err(FileSystemError::NotSupported);
        }
        // Проверка прав не оставляет за собой маппинг.
        let probe = region
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
            .map_err(no_space)?;
        drop(probe);
        let size = info.size_bytes;
        let id = self.insert(Store::Mutable(region))?;
        Ok((id, size))
    }

    pub(crate) fn slice(
        &self,
        window: Window,
        mode: AccessMode,
        allow_duplicate: bool,
    ) -> Result<MemoryRegion, FileSystemError> {
        let store = self.store(window.store)?;
        let region = match store {
            Store::Initial(image) => image.region(),
            Store::Mutable(region) => region,
        };

        let info = region.inspect().map_err(no_space)?;
        let size = window.size.div_ceil(USERLAND_PAGE_SIZE) * USERLAND_PAGE_SIZE;
        if matches!(store, Store::Initial(_)) {
            return region
                .slice(window.offset, size, access_for(mode, info.access))
                .map_err(no_space);
        }
        if window.offset != 0 || size != info.size_bytes {
            return Err(FileSystemError::NotSupported);
        }

        // Клиент получает производную без DUPLICATE; корень остаётся у сервера и отзывает её.
        region
            .duplicate(window_rights(mode.writable(), allow_duplicate), None)
            .map_err(no_space)
    }

    pub(crate) fn copy_from(
        &self,
        target: Window,
        source: &MemoryRegion,
        offset: u64,
        len: usize,
    ) -> Result<(), FileSystemError> {
        let source = Mapping::full(source, UserMemFlags::ReadOnly)
            .map_err(|refused| no_space(refused.error))?;
        self.copy_from_pages(target, offset, &source, len)
    }

    pub(crate) fn copy_from_pages(
        &self,
        target: Window,
        offset: u64,
        source: &MappedPages,
        len: usize,
    ) -> Result<(), FileSystemError> {
        let offset = window_offset(target, offset)?;
        self.with_mapping(target.store, UserMemFlags::ReadWrite, |target| {
            source.copy_to(0, target, offset, len).map_err(no_space)
        })
    }

    pub(crate) fn copy_into_pages(
        &self,
        source: Window,
        offset: u64,
        target: &MappedPages,
        len: usize,
    ) -> Result<(), FileSystemError> {
        let offset = window_offset(source, offset)?;
        if let Store::Initial(image) = self.store(source.store)? {
            return target
                .copy_in_at(0, image_bytes(image, offset, len)?)
                .map_err(no_space);
        }

        self.with_mapping(source.store, UserMemFlags::ReadOnly, |source| {
            source.copy_to(offset, target, 0, len).map_err(no_space)
        })
    }

    fn insert(&mut self, bytes: Store) -> Result<StoreId, FileSystemError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(FileSystemError::NoSpace)?;
        insert_store(
            &mut self.stores,
            Stored {
                generation: self.generation,
                bytes,
            },
        )
    }

    pub(crate) fn generation(&self, id: StoreId) -> Result<u64, FileSystemError> {
        self.stores
            .get(id.raw() as usize)
            .and_then(Option::as_ref)
            .map(|store| store.generation)
            .ok_or(FileSystemError::NotFound)
    }

    fn store(&self, id: StoreId) -> Result<&Store, FileSystemError> {
        self.stores
            .get(id.raw() as usize)
            .and_then(Option::as_ref)
            .map(|store| &store.bytes)
            .ok_or(FileSystemError::NotFound)
    }

    fn with_mapping<T>(
        &self,
        id: StoreId,
        flags: UserMemFlags,
        access: impl FnOnce(&MappedPages) -> Result<T, FileSystemError>,
    ) -> Result<T, FileSystemError> {
        match self.store(id)? {
            Store::Mutable(region) => {
                let mapping =
                    Mapping::full(region, flags).map_err(|refused| no_space(refused.error))?;
                access(&mapping)
            }
            // Маска доступа образа не допускает запись.
            Store::Initial(_) => Err(no_space(kernel_objects::Error::Syscall(
                SyscallError::AccessDenied,
            ))),
        }
    }
}

impl ByteStore for StoreTable {
    fn allocate(&mut self, size: usize) -> Result<StoreId, FileSystemError> {
        let size = u64::try_from(size).map_err(|_| FileSystemError::NoSpace)?;
        if size == 0 || !size.is_multiple_of(USERLAND_PAGE_SIZE) {
            return Err(FileSystemError::NoSpace);
        }

        let region = self
            .resource
            .create_virtual(size, MemoryAccess::RW)
            .map_err(no_space)?;
        self.insert(Store::Mutable(region))
    }

    fn read(&self, store: StoreId, offset: usize, out: &mut [u8]) -> Result<(), FileSystemError> {
        if let Store::Initial(image) = self.store(store)? {
            out.copy_from_slice(image_bytes(image, offset, out.len())?);
            return Ok(());
        }

        self.with_mapping(store, UserMemFlags::ReadOnly, |mapping| {
            mapping.copy_out_at(offset, out).map_err(no_space)
        })
    }

    fn write(&mut self, store: StoreId, offset: usize, data: &[u8]) -> Result<(), FileSystemError> {
        self.with_mapping(store, UserMemFlags::ReadWrite, |mapping| {
            mapping.copy_in_at(offset, data).map_err(no_space)
        })
    }

    fn copy(
        &mut self,
        source: StoreId,
        source_offset: usize,
        target: StoreId,
        target_offset: usize,
        len: usize,
    ) -> Result<(), FileSystemError> {
        if source == target {
            return Err(FileSystemError::NoSpace);
        }

        if let Store::Initial(image) = self.store(source)? {
            let bytes = image_bytes(image, source_offset, len)?;
            return self.with_mapping(target, UserMemFlags::ReadWrite, |target| {
                target.copy_in_at(target_offset, bytes).map_err(no_space)
            });
        }

        self.with_mapping(source, UserMemFlags::ReadOnly, |source| {
            self.with_mapping(target, UserMemFlags::ReadWrite, |target| {
                source
                    .copy_to(source_offset, target, target_offset, len)
                    .map_err(no_space)
            })
        })
    }

    fn zero(&mut self, store: StoreId, offset: usize, len: usize) -> Result<(), FileSystemError> {
        self.with_mapping(store, UserMemFlags::ReadWrite, |mapping| {
            mapping.zero_at(offset, len).map_err(no_space)
        })
    }

    fn release(&mut self, store: StoreId) {
        if let Some(slot) = self.stores.get_mut(store.raw() as usize) {
            *slot = None;
        }
    }
}

fn image_bytes(
    image: &ReadOnlyMapping,
    offset: usize,
    len: usize,
) -> Result<&[u8], FileSystemError> {
    offset
        .checked_add(len)
        .and_then(|end| image.as_bytes().get(offset..end))
        .ok_or_else(|| no_space(kernel_objects::Error::OutOfBounds))
}

fn window_offset(window: Window, offset: u64) -> Result<usize, FileSystemError> {
    usize::try_from(
        window
            .offset
            .checked_add(offset)
            .ok_or(FileSystemError::NoSpace)?,
    )
    .map_err(|_| FileSystemError::NoSpace)
}

pub(crate) fn window_rights(writable: bool, duplicate: bool) -> Rights {
    let mut rights = Rights::READ | Rights::TRANSFER;
    if writable {
        rights |= Rights::WRITE;
    }

    if duplicate {
        rights |= Rights::DUPLICATE;
    }

    rights
}
