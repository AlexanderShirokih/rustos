use aarch64_paging::level::{L0, Level};
use aarch64_paging::mapper::{MapError, PageMapper};
use aarch64_paging::mem_flags::MemFlags;
use aarch64_paging::page_table::PageTable;
use aarch64_paging::table_alloc::TableAlloc;
use core::cell::UnsafeCell;
use memory::aligned::{Address, Aligned};
use memory::frame_allocator::FrameAllocator;
use memory::memory_mapper::{MemoryMapper, MemoryMappingError};
use memory::physical_address::{PageAlignedAddress, PhysicalAddress};
use memory::virtual_address::{PageAlignedVirtualAddress, VirtualAddress};

/// Локальный адаптер: используем `FrameAllocator` как источник страниц под page tables.
struct FrameTableAlloc<'a, FA: FrameAllocator>(&'a FA);

impl<'a, FA: FrameAllocator> TableAlloc for FrameTableAlloc<'a, FA> {
    #[inline]
    fn alloc_table_page(&mut self) -> Option<PageAlignedAddress> {
        let frame = self.0.allocate_frame()?;
        Some(frame.page_address())
    }

    #[inline]
    unsafe fn table_ptr<L: Level>(&self, pa: PageAlignedAddress) -> *mut PageTable<L> {
        pa.as_u64() as *mut PageTable<L>
    }
}

/// Маппер памяти для AArch64.
pub struct Aarch64MemoryMapper<'a, FA: FrameAllocator> {
    frame_allocator: &'a FA,
    mem_flags: MemFlags,
    mapper: UnsafeCell<PageMapper<FrameTableAlloc<'a, FA>>>,
}

// SAFETY: доступ к frame allocator должен быть синхронизирован снаружи.
unsafe impl<'a, FA: FrameAllocator + Sync> Sync for Aarch64MemoryMapper<'a, FA> {}

impl<'a, FA: FrameAllocator> Aarch64MemoryMapper<'a, FA> {
    pub fn new(frame_allocator: &'a FA, root_ptr: *mut PageTable<L0>, mem_flags: MemFlags) -> Self {

        Self {
            frame_allocator,
            mem_flags,
            mapper: UnsafeCell::new(PageMapper::new(root_ptr, FrameTableAlloc(&frame_allocator))),
        }
    }

    fn map_single(
        &mut self,
        phys: PageAlignedAddress,
        virt: PageAlignedVirtualAddress,
        mem_flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        self.mapper
            .get_mut()
            .map_page(virt, phys, mem_flags)
            .map_err(|e| match e {
                MapError::OccupiedByLeaf => MemoryMappingError::AlreadyMapped,
                _ => MemoryMappingError::VirtualMappingError,
            })
    }
}

impl<'a, FA: FrameAllocator> MemoryMapper for Aarch64MemoryMapper<'a, FA> {
    fn map_frames(
        &mut self,
        start_address: &PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<(), MemoryMappingError> {
        if size == 0 {
            return Ok(());
        }

        let page_size = PageAlignedVirtualAddress::ALIGNMENT;
        let page_count = (size + page_size - 1) / page_size;

        for i in 0..page_count {
            let virt = VirtualAddress::new(start_address.as_usize() + i * page_size);
            let phys = self
                .frame_allocator
                .allocate_frame()
                .map(|frame| frame.page_address())
                .ok_or_else(|| MemoryMappingError::OutOfMemory)?;

            self.map_single(
                phys,
                PageAlignedVirtualAddress::new_unchecked(virt),
                self.mem_flags,
            )?;
        }

        Ok(())
    }

    fn map_exact(
        &mut self,
        source_address: PageAlignedAddress,
        target_address: PageAlignedVirtualAddress,
        size: usize,
        mem_flags: u64,
    ) -> Result<(), MemoryMappingError> {
        let page_size = source_address.alignment();
        let page_count = (size + page_size - 1) / page_size;

        for page in 0..page_count {
            let offset = page * page_size;

            let virt = VirtualAddress::new(target_address.as_usize() + offset);
            let phys = PhysicalAddress::new(source_address.as_usize() + offset);

            self.map_single(
                PageAlignedAddress::new_unchecked(phys),
                PageAlignedVirtualAddress::new_unchecked(virt),
                MemFlags::from_bits(mem_flags),
            )?;
        }

        Ok(())
    }
}
