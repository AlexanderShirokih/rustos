//! Маппинг виртуальных адресов на физические для AArch64.

use aarch64_paging::level::{L0, L1, L2, L3, Level};
use aarch64_paging::mapper::{MapError, MapLeaf, PageMapper};
use aarch64_paging::mem_flags::Aarch64MemFlags;
use aarch64_paging::page_table::PageTable;
use aarch64_paging::table_alloc::TableAlloc;
use collections::LockCell;
use memory::MemFlags;
use memory::aligned::Aligned;
use memory::frame_allocator::FrameAllocator;
use memory::memory_mapper::{MemoryMapper, MemoryMappingError};
use memory::physical_address::{AlignedPhysicalAddress, PageAlignedAddress, PhysicalAddress};
use memory::virtual_address::{AlignedVirtualAddress, PageAlignedVirtualAddress, VirtualAddress};

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
    /// Внутренний маппер таблиц страниц.
    mapper: L,
}

impl<'a, FA, L> Aarch64MemoryMapper<'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
    /// Создаёт маппер с identity mapping.
    pub fn new(
        frame_allocator: &'a FA,
        root_ptr: *mut PageTable<L0>,
        mem_flags: Aarch64MemFlags,
    ) -> Self {
        Self::new_with_offset(frame_allocator, root_ptr, mem_flags, 0)
    }

    pub fn new_with_offset(
        frame_allocator: &'a FA,
        root_ptr: *mut PageTable<L0>,
        mem_flags: Aarch64MemFlags,
        vaddr_offset: usize,
    ) -> Self {
        Self {
            frame_allocator,
            mem_flags,
            mapper: L::new(PageMapper::new(
                root_ptr,
                FrameTableAlloc::new(frame_allocator, vaddr_offset),
            )),
        }
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

        self.mapper.with_lock(|mapper| {
            let mut remaining = total_size;
            let mut virt = source_address.as_virtual();
            let mut phys = target_address.as_physical_address();

            let size_1g = 1usize << L1::SHIFT;
            let size_2m = 1usize << L2::SHIFT;
            let size_4k = 1usize << L3::SHIFT;

            while remaining != 0 {
                if remaining >= size_1g
                    && let (Some(v1g), Some(p1g)) = (
                        AlignedVirtualAddress::<{ L1::SHIFT }>::new(virt),
                        AlignedPhysicalAddress::<{ L1::SHIFT }>::new(phys),
                    )
                {
                    map_contiguous_inner::<FA, { L1::SHIFT }, _>(mapper, v1g, p1g, mem_flags)?;
                    virt = virt.offset(size_1g);
                    phys = phys.add(size_1g);
                    remaining -= size_1g;
                    continue;
                }

                if remaining >= size_2m
                    && let (Some(v2m), Some(p2m)) = (
                        AlignedVirtualAddress::<{ L2::SHIFT }>::new(virt),
                        AlignedPhysicalAddress::<{ L2::SHIFT }>::new(phys),
                    )
                {
                    map_contiguous_inner::<FA, { L2::SHIFT }, _>(mapper, v2m, p2m, mem_flags)?;
                    virt = virt.offset(size_2m);
                    phys = phys.add(size_2m);
                    remaining -= size_2m;
                    continue;
                }

                let v4k = PageAlignedVirtualAddress::new_unchecked(virt);
                let p4k = PageAlignedAddress::new_unchecked(phys);
                map_contiguous_inner::<FA, { L3::SHIFT }, _>(mapper, v4k, p4k, mem_flags)?;
                virt = virt.offset(size_4k);
                phys = phys.add(size_4k);
                remaining -= size_4k;
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
        let mem_flags = self.mem_flags;
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

    fn unmap(&self, _address: PageAlignedVirtualAddress, _size: usize) -> Result<(), ()> {
        // TODO: решить вопрос с unmapping
        Ok(())
    }
}
