//! Маппинг виртуальных адресов на физические для AArch64.

use core::sync::atomic::{AtomicU64, Ordering};

use collections::LockCell;
use hal_aarch64_paging::{
    level::{L0, L1, L2, L3, Level},
    mapper::{self, MapError, MapLeaf, PageMapper, WalkError},
    mem_flags::Aarch64MemFlags,
    page_table::PageTable,
    table_alloc::TableAlloc,
};
use memory::{
    MemFlags,
    aligned::Aligned,
    frame_allocator::FrameAllocator,
    memory_mapper::{
        AddressSpaceHandle, AddressSpaceTag, MemoryMapper, MemoryMappingError,
        MemoryRemappingError, MemoryUnmappingError,
    },
    physical_address::{AlignedPhysicalAddress, PageAlignedAddress, PhysicalAddress},
    virtual_address::{AlignedVirtualAddress, PageAlignedVirtualAddress, VirtualAddress},
};

use crate::memory::{
    asid::{self, unpack_asid},
    regs::{common::EL1, tlb::TranslationLookasideBuffer},
};

/// Адаптер FrameAllocator для выделения таблиц страниц.
///
/// Использует identity mapping (VA = PA) до включения MMU.
pub(crate) struct FrameTableAlloc<'a, FA: FrameAllocator> {
    /// Базовый аллокатор фреймов.
    allocator: &'a FA,
    /// Смещение для VA относительно PA (0 для identity).
    vaddr_offset: usize,
}

impl<'a, FA: FrameAllocator> FrameTableAlloc<'a, FA> {
    /// Создаёт аллокатор с линейным маппингом: VA = PA + offset.
    pub fn new(allocator: &'a FA, offset: usize) -> Self {
        Self {
            allocator,
            vaddr_offset: offset,
        }
    }
}

impl<FA: FrameAllocator> TableAlloc for FrameTableAlloc<'_, FA> {
    fn alloc_table_page(&mut self) -> Option<PageAlignedAddress> {
        let frame = self.allocator.allocate_frame()?;
        Some(frame.page_address())
    }

    unsafe fn table_ptr<L: Level>(
        &self,
        pa: PageAlignedAddress,
        _target_va: usize,
    ) -> *mut PageTable<L> {
        // Линейное отображение таблиц страниц
        (pa.as_usize() + self.vaddr_offset) as *mut PageTable<L>
    }
}

/// Какому адресному пространству принадлежит маппер.
///
/// Различение влияет на `nG` leaf-страниц (kernel - global, user - per-ASID)
/// и на стратегию TLB-инвалидации в `remap`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AddressSpaceKind {
    Kernel,
    User,
}

/// Маппер памяти для AArch64.
pub struct Aarch64MemoryMapper<'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
    /// Аллокатор физических фреймов.
    frame_allocator: &'a FA,
    /// Флаги памяти по умолчанию.
    mem_flags: Aarch64MemFlags,
    /// Физический адрес корня таблиц.
    root_pa: PhysicalAddress,
    /// Внутренний маппер таблиц страниц.
    mapper: L,
    /// Текущий тег AS (`0` - никогда не активирован). Lazy-allocated на пути
    /// `activate_handle`.
    asid_tag: AtomicU64,
    /// Kernel или user - определяет `nG` leaf-страниц и стратегию TLB-flush.
    kind: AddressSpaceKind,
}

// SAFETY: доступ к page-tables сериализован `LockCell`, frame-аллокатор синхронизирует
// `&self`-методы внутри.
unsafe impl<'a, FA, L> Send for Aarch64MemoryMapper<'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
}
// SAFETY: см. комментарий к `Send`.
unsafe impl<'a, FA, L> Sync for Aarch64MemoryMapper<'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
}

impl<'a, FA, L> Aarch64MemoryMapper<'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
    /// Создаёт маппер с заданным offset для трансляции PA->VA таблиц
    /// (`0` - identity mapping для boot до включения MMU).
    ///
    /// `kind` определяет роль AS: kernel-маппинги global, user-маппинги
    /// получают `nG=1` и per-AS ASID-тег на пути `activate_handle`.
    pub fn new_with_offset(
        frame_allocator: &'a FA,
        root_ptr: *mut PageTable<L0>,
        mem_flags: Aarch64MemFlags,
        vaddr_offset: usize,
        kind: AddressSpaceKind,
    ) -> Self {
        // PA = VA - vaddr_offset. До MMU vaddr_offset=0 (identity), после MMU
        // - higher_half_base. Для свежевыделенных user-root таблиц передаётся
        // PA напрямую (см. конструктор фабрики).
        let root_pa = PhysicalAddress::new((root_ptr as usize).wrapping_sub(vaddr_offset));
        Self {
            frame_allocator,
            mem_flags,
            root_pa,
            mapper: L::new(PageMapper::new(
                root_ptr,
                FrameTableAlloc::new(frame_allocator, vaddr_offset),
            )),
            asid_tag: AtomicU64::new(0),
            kind,
        }
    }

    fn leaf_flags(&self, base: Aarch64MemFlags) -> Aarch64MemFlags {
        base.ng(matches!(self.kind, AddressSpaceKind::User))
    }

    pub fn map_exact_impl(
        &self,
        source_address: PageAlignedVirtualAddress,
        target_address: PageAlignedAddress,
        size: usize,
        mem_flags: Aarch64MemFlags,
    ) -> Result<(), MemoryMappingError> {
        if size == 0 {
            return Ok(());
        }

        let page_size = PageAlignedAddress::ALIGNMENT;
        let total_size = size.div_ceil(page_size) * page_size;
        let mem_flags = self.leaf_flags(mem_flags);

        self.mapper.with_lock(|mapper| {
            let mut remaining = total_size;
            let mut virt = source_address.as_virtual();
            let mut phys = target_address.as_physical_address();

            let l1_block = 1usize << L1::SHIFT;
            let l2_block = 1usize << L2::SHIFT;
            let l3_page = 1usize << L3::SHIFT;

            while remaining != 0 {
                if remaining >= l1_block
                    && let (Some(v1g), Some(p1g)) = (
                        AlignedVirtualAddress::<{ L1::SHIFT }>::new(virt),
                        AlignedPhysicalAddress::<{ L1::SHIFT }>::new(phys),
                    )
                {
                    map_contiguous_inner::<FA, { L1::SHIFT }, _>(mapper, v1g, p1g, mem_flags)?;
                    virt = virt.offset(l1_block);
                    phys = phys.add(l1_block);
                    remaining -= l1_block;
                    continue;
                }

                if remaining >= l2_block
                    && let (Some(v2m), Some(p2m)) = (
                        AlignedVirtualAddress::<{ L2::SHIFT }>::new(virt),
                        AlignedPhysicalAddress::<{ L2::SHIFT }>::new(phys),
                    )
                {
                    map_contiguous_inner::<FA, { L2::SHIFT }, _>(mapper, v2m, p2m, mem_flags)?;
                    virt = virt.offset(l2_block);
                    phys = phys.add(l2_block);
                    remaining -= l2_block;
                    continue;
                }

                let v4k = PageAlignedVirtualAddress::new_unchecked(virt);
                let p4k = PageAlignedAddress::new_unchecked(phys);
                map_contiguous_inner::<FA, { L3::SHIFT }, _>(mapper, v4k, p4k, mem_flags)?;
                virt = virt.offset(l3_page);
                phys = phys.add(l3_page);
                remaining -= l3_page;
            }

            Ok(())
        })
    }
}

fn map_contiguous_inner<FA: FrameAllocator, const SHIFT: u8, P>(
    mapper: &mut PageMapper<FrameTableAlloc<'_, FA>>,
    virt: AlignedVirtualAddress<SHIFT>,
    phys: P,
    mem_flags: Aarch64MemFlags,
) -> Result<(), MemoryMappingError>
where
    P: MapLeaf<SHIFT> + Into<PhysicalAddress>,
{
    match mapper.map_page(virt, phys, mem_flags) {
        Ok(()) => Ok(()),

        Err(MapError::NeedsSmallerPages) if SHIFT == L1::SHIFT => {
            let mut v = AlignedVirtualAddress::<{ L2::SHIFT }>::new_unchecked(virt.into());
            let mut p = AlignedPhysicalAddress::<{ L2::SHIFT }>::new_unchecked(phys.into());

            for _ in 0..512 {
                map_contiguous_inner::<FA, { L2::SHIFT }, _>(mapper, v, p, mem_flags)?;
                v = v.next_aligned();
                p = p.next_aligned();
            }
            Ok(())
        }

        Err(MapError::NeedsSmallerPages) if SHIFT == L2::SHIFT => {
            let mut v = AlignedVirtualAddress::<{ L3::SHIFT }>::new_unchecked(virt.into());
            let mut p = AlignedPhysicalAddress::<{ L3::SHIFT }>::new_unchecked(phys.into());

            for _ in 0..512 {
                map_contiguous_inner::<FA, { L3::SHIFT }, _>(mapper, v, p, mem_flags)?;
                v = v.next_aligned();
                p = p.next_aligned();
            }
            Ok(())
        }

        Err(MapError::AlreadyMapped) => Err(MemoryMappingError::AlreadyMapped),
        Err(MapError::OutOfMemory) => Err(MemoryMappingError::OutOfMemory),
        Err(_) => Err(MemoryMappingError::VirtualMappingError),
    }
}

impl<'a, FA, L> MemoryMapper for Aarch64MemoryMapper<'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
    fn map(
        &self,
        start_address: &PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<(), MemoryMappingError> {
        if size == 0 {
            return Ok(());
        }

        let page_size = PageAlignedVirtualAddress::ALIGNMENT;
        let page_count = size.div_ceil(page_size);
        let mem_flags = self.leaf_flags(self.mem_flags);
        let frame_allocator = self.frame_allocator;

        self.mapper.with_lock(|mapper| {
            for i in 0..page_count {
                let virt = VirtualAddress::new(start_address.as_usize() + i * page_size);
                let phys = frame_allocator
                    .allocate_frame()
                    .map(|frame| frame.page_address())
                    .ok_or(MemoryMappingError::OutOfMemory)?;

                map_contiguous_inner::<FA, { L3::SHIFT }, _>(
                    mapper,
                    PageAlignedVirtualAddress::new_unchecked(virt),
                    phys,
                    mem_flags,
                )?;
            }
            Ok(())
        })
    }

    fn map_exact(
        &self,
        source_address: PageAlignedVirtualAddress,
        target_address: PageAlignedAddress,
        size: usize,
        mem_flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        self.map_exact_impl(
            source_address,
            target_address,
            size,
            Aarch64MemFlags::from_memflags(mem_flags),
        )
    }

    fn unmap(
        &self,
        _address: PageAlignedVirtualAddress,
        _size: usize,
    ) -> Result<(), MemoryUnmappingError> {
        // TODO: решить вопрос с unmapping
        Err(MemoryUnmappingError::Unsupported)
    }

    fn activate_handle(&self) -> AddressSpaceHandle {
        match self.kind {
            AddressSpaceKind::Kernel => {
                AddressSpaceHandle::new(self.root_pa, AddressSpaceTag::NONE)
            }
            AddressSpaceKind::User => {
                let (_, raw) = asid::acquire(&self.asid_tag);
                AddressSpaceHandle::new(self.root_pa, AddressSpaceTag(raw))
            }
        }
    }

    #[cfg(feature = "qemu-tests")]
    fn query_leaf_raw(&self, address: PageAlignedVirtualAddress) -> Option<u64> {
        self.mapper.with_lock(|mapper| {
            let (l3, idx) = mapper.walk_to_l3_leaf(address).ok()?;
            // SAFETY: walk_to_l3_leaf вернул валидный (l3, idx) для leaf-Page.
            Some(unsafe { (*l3).get_raw(idx) })
        })
    }

    fn remap(
        &self,
        start_address: PageAlignedVirtualAddress,
        size: usize,
        new_flags: MemFlags,
    ) -> Result<(), MemoryRemappingError> {
        if size == 0 {
            return Ok(());
        }
        let page_size = PageAlignedVirtualAddress::ALIGNMENT;
        if !size.is_multiple_of(page_size) {
            return Err(MemoryRemappingError::MisalignedRange);
        }
        let aarch64_flags = self.leaf_flags(Aarch64MemFlags::from_memflags(new_flags));
        let asid = unpack_asid(self.asid_tag.load(Ordering::Acquire));

        self.mapper.with_lock(|mapper| {
            let mut va = start_address.as_virtual();
            let mut left = size;
            while left != 0 {
                let page = PageAlignedVirtualAddress::new_unchecked(va);
                let (l3, idx) = mapper
                    .walk_to_l3_leaf(page)
                    .map_err(|e| walk_to_remap_err(&e))?;
                // SAFETY: walk_to_l3_leaf вернул валидный (l3, idx) для leaf-Page;
                // mapper-lock держится этим with_lock - эксклюзивный доступ к таблицам.
                unsafe { mapper::update_l3_flags(l3, idx, aarch64_flags) }
                    .map_err(|e| walk_to_remap_err(&e))?;

                match self.kind {
                    AddressSpaceKind::User => {
                        // Если AS ещё ни разу не активировался (asid == 0),
                        // TLB для него заведомо пуст - `tlbi` не нужен.
                        if asid != 0 {
                            TranslationLookasideBuffer::<EL1>::invalidate_va_asid(
                                page.as_usize(),
                                asid,
                            );
                        }
                    }
                    AddressSpaceKind::Kernel => {
                        // Kernel-маппинги global - инвалидируем VA во всех ASID.
                        TranslationLookasideBuffer::<EL1>::invalidate_va_global(page.as_usize());
                    }
                }

                va = va.offset(page_size);
                left -= page_size;
            }
            Ok::<(), MemoryRemappingError>(())
        })
    }
}

fn walk_to_remap_err(err: &WalkError) -> MemoryRemappingError {
    match err {
        WalkError::HitBlock => MemoryRemappingError::UnsupportedBlockMapping,
        // Decode error в leaf-walk означает мусор в дескрипторе - для caller'а
        // практически неотличимо от Invalid: семантически "страница не замаплена".
        WalkError::NotMapped | WalkError::Decode(_) => MemoryRemappingError::NotMapped,
    }
}
