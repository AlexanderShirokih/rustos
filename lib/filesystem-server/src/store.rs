//! Таблица регионов, на которых лежат байты файловых бекендов.

use alloc::vec::Vec;

use filesystem::{ByteStore, StoreId, Window, insert_store};
use filesystem_abi::{AccessMode, FileSystemError};
use kernel_objects::{Mapping, MemoryRegion, OwnedHandle, RegionKind, Resource};
use runtime::{MemoryAccess, UserMemFlags};
use syscall::Rights;
use userland::USERLAND_PAGE_SIZE;

use crate::{error::no_space, service::access_for};

enum Store {
    Initial(MemoryRegion),
    Owned {
        region: MemoryRegion,
        mapping: Mapping,
    },
    Imported {
        region: MemoryRegion,
        mapping: Option<Mapping>,
    },
}

pub(crate) struct StoreTable {
    stores: Vec<Option<Store>>,
    resource: Resource,
}

impl StoreTable {
    pub(crate) fn new(resource: Resource, initial: Option<MemoryRegion>) -> Self {
        let mut stores = Vec::new();
        // Регион точки монтирования ложится первым: окна бекенда адресуют его как `StoreId::MOUNT`.
        if let Some(region) = initial {
            stores.push(Some(Store::Initial(region)));
        }
        Self { stores, resource }
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
            .handle()
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
            .map_err(no_space)?;
        drop(probe);
        // Маппинг живёт вместе с именем. Отказ ядра регистрацию не срывает: такой регион
        // отображается на время операции.
        let mapping = region.map_full(UserMemFlags::ReadWrite).ok();

        let size = info.size_bytes;
        let id = self.insert(Store::Imported { region, mapping })?;
        Ok((id, size))
    }

    pub(crate) fn slice(
        &self,
        window: Window,
        mode: AccessMode,
    ) -> Result<OwnedHandle, FileSystemError> {
        let store = self.store(window.store)?;
        let region = match store {
            Store::Initial(region)
            | Store::Owned { region, .. }
            | Store::Imported { region, .. } => region,
        };
        let info = region.inspect().map_err(no_space)?;
        let size = window.size.div_ceil(USERLAND_PAGE_SIZE) * USERLAND_PAGE_SIZE;
        if matches!(store, Store::Initial(_)) {
            return region
                .slice(window.offset, size, access_for(mode, info.access))
                .map(MemoryRegion::into_handle)
                .map_err(no_space);
        }
        if window.offset != 0 || size != info.size_bytes {
            return Err(FileSystemError::NotSupported);
        }
        let mut rights = Rights::READ | Rights::TRANSFER;
        if mode.writable() {
            rights |= Rights::WRITE;
        }
        // Клиент получает производную без DUPLICATE; корень остаётся у сервера и отзывает её.
        region.handle().duplicate(rights, None).map_err(no_space)
    }

    fn insert(&mut self, store: Store) -> Result<StoreId, FileSystemError> {
        insert_store(&mut self.stores, store)
    }

    fn store(&self, id: StoreId) -> Result<&Store, FileSystemError> {
        self.stores
            .get(id.raw() as usize)
            .and_then(Option::as_ref)
            .ok_or(FileSystemError::NotFound)
    }

    fn with_mapping<T>(
        &self,
        id: StoreId,
        flags: UserMemFlags,
        access: impl FnOnce(&Mapping) -> Result<T, FileSystemError>,
    ) -> Result<T, FileSystemError> {
        match self.store(id)? {
            Store::Owned { mapping, .. }
            | Store::Imported {
                mapping: Some(mapping),
                ..
            } => access(mapping),
            Store::Initial(region) | Store::Imported { region, .. } => {
                let mapping = region.map_full(flags).map_err(no_space)?;
                access(&mapping)
            }
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
        let mapping = region
            .map(size, UserMemFlags::ReadWrite)
            .map_err(no_space)?;
        self.insert(Store::Owned { region, mapping })
    }

    fn read(&self, store: StoreId, offset: usize, out: &mut [u8]) -> Result<(), FileSystemError> {
        self.with_mapping(store, UserMemFlags::ReadOnly, |mapping| {
            mapping.copy_out_at(offset, out).map_err(no_space)
        })
    }

    fn write(&mut self, store: StoreId, offset: usize, data: &[u8]) -> Result<(), FileSystemError> {
        self.with_mapping(store, UserMemFlags::ReadWrite, |mapping| {
            mapping.copy_in_at(offset, data).map_err(no_space)
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
