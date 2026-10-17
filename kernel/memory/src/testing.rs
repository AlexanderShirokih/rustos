//! Маппер памяти для тестов ядра на хосте. Рабочий код его не использует.
//!
//! [`PageMapper`] держит отображённые страницы в словаре вместе с их байтами и [`MemFlags`] и
//! записывает каждую успешную операцию в журнал [`Call`]. [`PageMapperFactory`] выдаёт такие
//! мапперы с различными корнями таблиц и общим журналом.

use alloc::{
    boxed::Box,
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    any::Any,
    sync::atomic::{AtomicUsize, Ordering},
};

use spin::Mutex;

use crate::{
    MemFlags,
    mem_flags::AccessMode,
    memory_mapper::{
        AddressSpaceFactory, AddressSpaceHandle, AddressSpaceTag, AsCreateError, MemoryMapper,
        MemoryMappingError, MemoryRemappingError, MemoryUnmappingError, UserCopyError,
    },
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

const PAGE_SIZE: usize = crate::PAGE_SIZE.get();

/// Успешная операция [`PageMapper`] с аргументами вызова.
#[derive(Clone)]
pub enum Call {
    Map(MapCall),
    MapExact(MapExactCall),
    Unmap(UnmapCall),
    Remap {
        va: usize,
        size: usize,
        flags: MemFlags,
    },
    ZeroOwnedFrame {
        pa: usize,
    },
}

#[derive(Clone)]
pub struct MapCall {
    pub va: usize,
    pub page_count: usize,
    pub init: Vec<u8>,
    pub flags: MemFlags,
}

#[derive(Clone)]
pub struct MapExactCall {
    pub va: usize,
    pub pa: usize,
    pub size: usize,
    pub flags: MemFlags,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnmapCall {
    pub va: usize,
    pub size: usize,
}

/// Маппер, который держит страницы адресного пространства в памяти хоста.
///
/// Операции отказывают так же, как маппер ядра: повторное отображение, снятие и перемаппинг
/// неотображённой страницы, копирование без user-прав. Страница `map_exact` читается как
/// занулённая.
pub struct PageMapper {
    root: PhysicalAddress,
    pages: Mutex<BTreeMap<usize, Page>>,
    journal: Arc<Mutex<Vec<Call>>>,
    map_budget: AtomicUsize,
    map_exact_budget: AtomicUsize,
}

struct Page {
    bytes: Box<[u8]>,
    flags: MemFlags,
}

/// Фабрика [`PageMapper`] с общим журналом и учётом освобождённых мапперов.
///
/// Корень таблиц `n`-го маппера равен `BASE_ROOT_PA + n * PAGE_SIZE`.
pub struct PageMapperFactory {
    mappers: Mutex<Vec<Weak<PageMapper>>>,
    journal: Arc<Mutex<Vec<Call>>>,
    map_exact_budget: usize,
}

impl PageMapper {
    #[must_use]
    pub fn new() -> Self {
        Self::with_root(PhysicalAddress::new(0), Arc::default())
    }

    /// Маппер с одним отображением, заданным аргументами [`MemoryMapper::map`].
    pub fn with_mapping(
        va: usize,
        page_count: usize,
        init: &[u8],
        flags: MemFlags,
    ) -> Result<Arc<Self>, MemoryMappingError> {
        let va = PageAlignedVirtualAddress::from_usize(va)
            .ok_or(MemoryMappingError::VirtualMappingError)?;
        let mapper = Self::new();
        mapper.map(va, page_count, init, flags)?;
        Ok(Arc::new(mapper))
    }

    fn with_root(root: PhysicalAddress, journal: Arc<Mutex<Vec<Call>>>) -> Self {
        Self {
            root,
            pages: Mutex::new(BTreeMap::new()),
            journal,
            map_budget: AtomicUsize::new(usize::MAX),
            map_exact_budget: AtomicUsize::new(usize::MAX),
        }
    }

    /// Журнал успешных операций в порядке вызова.
    pub fn calls(&self) -> Vec<Call> {
        self.journal.lock().clone()
    }

    pub fn maps(&self) -> Vec<MapCall> {
        maps(&self.journal.lock())
    }

    pub fn map_exacts(&self) -> Vec<MapExactCall> {
        self.journal
            .lock()
            .iter()
            .filter_map(|call| match call {
                Call::MapExact(call) => Some(call.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn unmaps(&self) -> Vec<UnmapCall> {
        self.journal
            .lock()
            .iter()
            .filter_map(|call| match call {
                Call::Unmap(call) => Some(*call),
                _ => None,
            })
            .collect()
    }

    /// PA фреймов, переданных в `zero_owned_frame`.
    pub fn zeroed_frames(&self) -> Vec<usize> {
        self.journal
            .lock()
            .iter()
            .filter_map(|call| match call {
                Call::ZeroOwnedFrame { pa } => Some(*pa),
                _ => None,
            })
            .collect()
    }

    /// После `count` успешных вызовов `map` отказывает [`MemoryMappingError::OutOfMemory`].
    pub fn fail_map_after(&self, count: usize) {
        self.map_budget.store(count, Ordering::Relaxed);
    }

    /// После `count` успешных вызовов `map_exact` отказывает
    /// [`MemoryMappingError::OutOfMemory`].
    pub fn fail_map_exact_after(&self, count: usize) {
        self.map_exact_budget.store(count, Ordering::Relaxed);
    }

    fn insert(
        &self,
        base: usize,
        size: usize,
        init: &[u8],
        flags: MemFlags,
        budget: &AtomicUsize,
    ) -> Result<(), MemoryMappingError> {
        base.checked_add(size)
            .ok_or(MemoryMappingError::VirtualMappingError)?;

        let mut pages = self.pages.lock();
        if pages.range(base..base + size).next().is_some() {
            return Err(MemoryMappingError::AlreadyMapped);
        }
        if !spend(budget) {
            return Err(MemoryMappingError::OutOfMemory);
        }

        for page_base in (base..base + size).step_by(PAGE_SIZE) {
            let mut bytes = vec![0u8; PAGE_SIZE].into_boxed_slice();
            let source = init.get(page_base - base..).unwrap_or_default();
            for (target, byte) in bytes.iter_mut().zip(source) {
                *target = *byte;
            }
            pages.insert(page_base, Page { bytes, flags });
        }
        Ok(())
    }

    fn user_page(
        pages: &BTreeMap<usize, Page>,
        va: VirtualAddress,
        offset: usize,
        write: bool,
    ) -> Result<(usize, usize), UserCopyError> {
        let address = va
            .as_usize()
            .checked_add(offset)
            .ok_or(UserCopyError::NotMapped)?;
        let page_base = address - address % PAGE_SIZE;
        let page = pages.get(&page_base).ok_or(UserCopyError::NotMapped)?;

        let allowed = match user_copy_access(page.flags) {
            AccessMode::None => false,
            AccessMode::Readonly => !write,
            AccessMode::Writable => true,
        };
        if !allowed {
            return Err(UserCopyError::AccessDenied);
        }
        Ok((page_base, address % PAGE_SIZE))
    }

    fn record(&self, call: Call) {
        self.journal.lock().push(call);
    }
}

impl Default for PageMapper {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryMapper for PageMapper {
    fn map(
        &self,
        va: PageAlignedVirtualAddress,
        page_count: usize,
        init: &[u8],
        flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        let size = page_count
            .checked_mul(PAGE_SIZE)
            .ok_or(MemoryMappingError::VirtualMappingError)?;
        if init.len() > size {
            return Err(MemoryMappingError::InitExceedsMapping);
        }

        self.insert(va.as_usize(), size, init, flags, &self.map_budget)?;
        self.record(Call::Map(MapCall {
            va: va.as_usize(),
            page_count,
            init: init.to_vec(),
            flags,
        }));
        Ok(())
    }

    fn map_exact(
        &self,
        source_address: PageAlignedVirtualAddress,
        target_address: PageAlignedAddress,
        size: usize,
        mem_flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        let mapped_size = size
            .div_ceil(PAGE_SIZE)
            .checked_mul(PAGE_SIZE)
            .ok_or(MemoryMappingError::VirtualMappingError)?;

        self.insert(
            source_address.as_usize(),
            mapped_size,
            &[],
            mem_flags,
            &self.map_exact_budget,
        )?;
        self.record(Call::MapExact(MapExactCall {
            va: source_address.as_usize(),
            pa: target_address.as_usize(),
            size,
            flags: mem_flags,
        }));
        Ok(())
    }

    fn unmap(
        &self,
        address: PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<(), MemoryUnmappingError> {
        if !size.is_multiple_of(PAGE_SIZE) {
            return Err(MemoryUnmappingError::MisalignedRange);
        }
        let base = address.as_usize();
        let end = base
            .checked_add(size)
            .ok_or(MemoryUnmappingError::NotMapped)?;

        let mut pages = self.pages.lock();
        for page_base in (base..end).step_by(PAGE_SIZE) {
            pages
                .remove(&page_base)
                .ok_or(MemoryUnmappingError::NotMapped)?;
        }
        drop(pages);

        self.record(Call::Unmap(UnmapCall { va: base, size }));
        Ok(())
    }

    fn remap(
        &self,
        start_address: PageAlignedVirtualAddress,
        size: usize,
        new_flags: MemFlags,
    ) -> Result<(), MemoryRemappingError> {
        if !size.is_multiple_of(PAGE_SIZE) {
            return Err(MemoryRemappingError::MisalignedRange);
        }
        let base = start_address.as_usize();
        let end = base
            .checked_add(size)
            .ok_or(MemoryRemappingError::NotMapped)?;

        let mut pages = self.pages.lock();
        for page_base in (base..end).step_by(PAGE_SIZE) {
            pages
                .get_mut(&page_base)
                .ok_or(MemoryRemappingError::NotMapped)?
                .flags = new_flags;
        }
        drop(pages);

        self.record(Call::Remap {
            va: base,
            size,
            flags: new_flags,
        });
        Ok(())
    }

    fn activate_handle(&self) -> AddressSpaceHandle {
        AddressSpaceHandle::new(self.root, AddressSpaceTag::NONE)
    }

    fn zero_owned_frame(&self, pa: PageAlignedAddress) {
        self.record(Call::ZeroOwnedFrame { pa: pa.as_usize() });
    }

    fn copy_user_in(&self, va: VirtualAddress, dst: &mut [u8]) -> Result<(), UserCopyError> {
        let pages = self.pages.lock();
        for (offset, target) in dst.iter_mut().enumerate() {
            let (page_base, index) = Self::user_page(&pages, va, offset, false)?;
            *target = pages
                .get(&page_base)
                .and_then(|page| page.bytes.get(index))
                .copied()
                .ok_or(UserCopyError::NotMapped)?;
        }
        Ok(())
    }

    fn copy_user_out(&self, va: VirtualAddress, src: &[u8]) -> Result<(), UserCopyError> {
        let mut pages = self.pages.lock();
        for (offset, byte) in src.iter().enumerate() {
            let (page_base, index) = Self::user_page(&pages, va, offset, true)?;
            *pages
                .get_mut(&page_base)
                .and_then(|page| page.bytes.get_mut(index))
                .ok_or(UserCopyError::NotMapped)? = *byte;
        }
        Ok(())
    }

    fn as_any(&self) -> &(dyn Any + 'static) {
        self
    }
}

impl PageMapperFactory {
    pub const BASE_ROOT_PA: usize = 0x10_0000;

    #[must_use]
    pub fn new() -> Self {
        Self::with_map_exact_budget(usize::MAX)
    }

    /// Фабрика, мапперы которой отказывают в каждом `map_exact`.
    #[must_use]
    pub fn failing_map_exact() -> Self {
        Self::with_map_exact_budget(0)
    }

    fn with_map_exact_budget(map_exact_budget: usize) -> Self {
        Self {
            mappers: Mutex::new(Vec::new()),
            journal: Arc::default(),
            map_exact_budget,
        }
    }

    pub fn created(&self) -> usize {
        self.mappers.lock().len()
    }

    pub fn released(&self) -> usize {
        self.mappers
            .lock()
            .iter()
            .filter(|mapper| mapper.strong_count() == 0)
            .count()
    }

    /// Журнал успешных операций всех выданных мапперов в порядке вызова.
    pub fn calls(&self) -> Vec<Call> {
        self.journal.lock().clone()
    }

    pub fn maps(&self) -> Vec<MapCall> {
        maps(&self.journal.lock())
    }
}

impl Default for PageMapperFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl AddressSpaceFactory for PageMapperFactory {
    fn create_user(&self) -> Result<Arc<dyn MemoryMapper + Send + Sync>, AsCreateError> {
        let mut mappers = self.mappers.lock();
        let root = mappers
            .len()
            .checked_mul(PAGE_SIZE)
            .and_then(|offset| offset.checked_add(Self::BASE_ROOT_PA))
            .ok_or(AsCreateError::OutOfMemory)?;

        let mapper = Arc::new(PageMapper::with_root(
            PhysicalAddress::new(root),
            self.journal.clone(),
        ));
        mapper.fail_map_exact_after(self.map_exact_budget);
        mappers.push(Arc::downgrade(&mapper));
        Ok(mapper)
    }
}

fn maps(journal: &[Call]) -> Vec<MapCall> {
    journal
        .iter()
        .filter_map(|call| match call {
            Call::Map(call) => Some(call.clone()),
            _ => None,
        })
        .collect()
}

fn spend(budget: &AtomicUsize) -> bool {
    budget
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| match left {
            0 => None,
            usize::MAX => Some(usize::MAX),
            left => Some(left - 1),
        })
        .is_ok()
}

fn user_copy_access(flags: MemFlags) -> AccessMode {
    match flags {
        MemFlags::Cached(owners) => owners.user.access,
        // Ядро копирует байты пользователя только через кэшируемую линейную карту.
        MemFlags::WriteCombining(_) | MemFlags::Device(_) => AccessMode::None,
    }
}
