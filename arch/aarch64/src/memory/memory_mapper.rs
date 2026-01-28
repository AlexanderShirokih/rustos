use aarch64_paging::level::{L0, L1, L2, L3, Level};
use aarch64_paging::mapper::{MapError, MapLeaf, PageMapper};
use aarch64_paging::mem_flags::MemFlags;
use aarch64_paging::page_table::PageTable;
use aarch64_paging::table_alloc::TableAlloc;
use core::cell::UnsafeCell;
use memory::aligned::{Address, Aligned};
use memory::frame_allocator::FrameAllocator;
use memory::memory_mapper::{MemoryMapper, MemoryMappingError};
use memory::physical_address::{AlignedPhysicalAddress, PageAlignedAddress, PhysicalAddress};
use memory::virtual_address::{AlignedVirtualAddress, PageAlignedVirtualAddress, VirtualAddress};

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

    fn map_contiguous<const SHIFT: u8, P>(
        &mut self,
        virt: AlignedVirtualAddress<SHIFT>,
        phys: P,
        mem_flags: MemFlags,
    ) -> Result<(), MemoryMappingError>
    where
        P: MapLeaf<SHIFT> + Into<PhysicalAddress>,
    {
        match self.mapper.get_mut().map_page(virt, phys, mem_flags) {
            Ok(()) => Ok(()),

            // Фоллбек с 1GB на 512 по 2MB
            Err(MapError::NeedsSmallerPages) if SHIFT == L1::SHIFT => {
                let mut v = AlignedVirtualAddress::<{ L2::SHIFT }>::new_unchecked(virt.into());
                let mut p = AlignedPhysicalAddress::<{ L2::SHIFT }>::new_unchecked(phys.into());

                for _ in 0..512 {
                    self.map_contiguous::<{ L2::SHIFT }, _>(v, p, mem_flags)?;
                    v = v.next_aligned();
                    p = p.next_aligned();
                }

                Ok(())
            }

            // Фоллбек с 2MB на 512 по 4KB
            Err(MapError::NeedsSmallerPages) if SHIFT == L2::SHIFT => {
                let mut v = AlignedVirtualAddress::<{ L3::SHIFT }>::new_unchecked(virt.into());
                let mut p = AlignedPhysicalAddress::<{ L3::SHIFT }>::new_unchecked(phys.into());

                for _ in 0..512 {
                    self.map_contiguous::<{ L3::SHIFT }, _>(v, p, mem_flags)?;
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
}

impl<'a, FA: FrameAllocator> MemoryMapper for Aarch64MemoryMapper<'a, FA> {
    fn map(
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

            self.map_contiguous(
                PageAlignedVirtualAddress::new_unchecked(virt),
                phys,
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
        if size == 0 {
            return Ok(());
        }

        let flags = MemFlags::from_bits(mem_flags);

        // map_exact работает минимум с 4KB гранулярностью
        let page_size = PageAlignedAddress::ALIGNMENT;
        let mut remaining = (size + page_size - 1) / page_size * page_size;

        let mut virt = VirtualAddress::new(target_address.as_usize());
        let mut phys = PhysicalAddress::new(source_address.as_usize());

        let size_1g = 1usize << L1::SHIFT;
        let size_2m = 1usize << L2::SHIFT;
        let size_4k = 1usize << L3::SHIFT;

        while remaining != 0 {
            // Пробуем 1GB блок, если влезает и оба адреса выровнены на 1GB.
            if remaining >= size_1g {
                if let (Some(v1g), Some(p1g)) = (
                    AlignedVirtualAddress::<{ L1::SHIFT }>::new(virt),
                    AlignedPhysicalAddress::<{ L1::SHIFT }>::new(phys),
                ) {
                    self.map_contiguous::<{ L1::SHIFT }, _>(v1g, p1g, flags)?;
                    virt = virt.offset(size_1g);
                    phys = phys.add(size_1g);
                    remaining -= size_1g;
                    continue;
                }
            }

            // Пробуем 2MB блок.
            if remaining >= size_2m {
                if let (Some(v2m), Some(p2m)) = (
                    AlignedVirtualAddress::<{ L2::SHIFT }>::new(virt),
                    AlignedPhysicalAddress::<{ L2::SHIFT }>::new(phys),
                ) {
                    self.map_contiguous::<{ L2::SHIFT }, _>(v2m, p2m, flags)?;
                    virt = virt.offset(size_2m);
                    phys = phys.add(size_2m);
                    remaining -= size_2m;
                    continue;
                }
            }

            // Иначе — 4KB.
            let v4k = PageAlignedVirtualAddress::new_unchecked(virt);
            let p4k = PageAlignedAddress::new_unchecked(phys);
            self.map_contiguous::<{ L3::SHIFT }, _>(v4k, p4k, flags)?;
            virt = virt.offset(size_4k);
            phys = phys.add(size_4k);
            remaining -= size_4k;
        }

        Ok(())
    }
}
