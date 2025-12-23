use crate::memory::layout::{MemoryLayout, MemoryRegion};
use crate::memory::memory_mapper::Aarch64MemoryMapper;
use crate::memory::mmu::{Mmu, NormalDualSpaceConfig, NormalSpaceConfig};
use crate::memory::ram_memory::Aarch64VirtualRamMemory;
use aarch64_paging::level::L0;
use aarch64_paging::mem_flags::MemFlags;
use aarch64_paging::page_table::PageTable;
use aarch64_paging::preset::KernelData;
use collections::{MutexCell, NoLockCell};
use memory::FrameBitmap;
use memory::aligned::{Address, Aligned};
use memory::frame_allocator::{FrameAllocator, PhysicalFrameAllocator};
use memory::heap_allocator::HeapAllocator;
use memory::memory_mapper::MemoryMapper;
use memory::memory_range::MemoryRange;
use memory::physical_address::{PageAlignedAddress, PhysicalAddress};
use memory::virtual_address::PageAlignedVirtualAddress;

pub struct Prepared {
    root_page: PageAlignedAddress,
    frame_allocator: PhysicalFrameAllocator<NoLockCell<FrameBitmap>>,
    identity_map_regions: alloc::vec::Vec<MemoryRegion<PageAlignedAddress>>,
}

pub struct Enabled {
    root_page: PageAlignedAddress,
    root_table: &'static mut PageTable<L0>,
    frame_allocator: PhysicalFrameAllocator<MutexCell<FrameBitmap>>,
}

pub struct HigherHalf {}

/// Центральный менеджер памяти, который владеет всеми компонентами системы памяти.
pub struct MemoryManager<Stage> {
    mem_flags: MemFlags,
    state: Stage,
}

impl MemoryManager<Prepared> {
    pub fn create(
        memory_layout: MemoryLayout,
    ) -> Result<MemoryManager<Prepared>, MemorySetupError> {
        let primary_heap = &memory_layout.heap().next();

        let heap = primary_heap.ok_or(MemorySetupError::NoHeapRegionFound)?;
        let heap_range: MemoryRange<PageAlignedAddress> = heap.clone().into();

        let identity_map_regions: alloc::vec::Vec<_> = memory_layout
            .iter()
            .filter(|&region| region.identity_map)
            .map(|region| region.clone())
            .collect();

        let identity_map_ranges = identity_map_regions
            .iter()
            .map(|region| MemoryRange::new(region.start, region.end))
            .collect();

        let frame_allocator = PhysicalFrameAllocator::new(&heap_range, &identity_map_ranges);

        let root_page = frame_allocator
            .allocate_frame()
            .map(|frame| frame.page_address())
            .ok_or(MemorySetupError::OutOfMemory)?;

        Ok(MemoryManager::<Prepared> {
            state: Prepared {
                frame_allocator,
                root_page,
                identity_map_regions,
            },
            mem_flags: heap.flags,
        })
    }

    pub fn enable(self) -> Result<MemoryManager<Enabled>, MemorySetupError> {
        let root_table = Self::create_root_table(self.state.root_page);

        let mut memory_mapper =
            Aarch64MemoryMapper::new(&self.state.frame_allocator, root_table, self.mem_flags);

        for region in self.state.identity_map_regions.iter() {
            let range = MemoryRange::new(region.start, region.end);

            debug_assert_eq!(range.size() % PageAlignedVirtualAddress::ALIGNMENT, 0);

            memory_mapper
                .map_exact(
                    range.start(),
                    &PageAlignedVirtualAddress::identity(range.start()),
                    range.size(),
                    region.flags.bits(),
                )
                .map_err(|_| MemorySetupError::OutOfMemory)?;
        }

        let regions = [
            (0x48100000, 0x48101000),
        ];

        for reg in regions.iter() {
            let (start, end) = *reg;
            let mem = MemoryRange::new(
                PageAlignedAddress::new_unchecked(PhysicalAddress::new(start)),
                PageAlignedAddress::new_unchecked(PhysicalAddress::new(end)),
            );

            memory_mapper
                .map_exact(
                    mem.start(),
                    &PageAlignedVirtualAddress::identity(mem.start()),
                    mem.size(),
                    KernelData::flags().bits(),
                )
                .map_err(|_| MemorySetupError::OutOfMemory)?;
        }

        // Включаем Memory Management Unit (MMU)
        // После включения весь доступ к памяти будет через виртуальные адреса
        Mmu::new().enable(NormalSpaceConfig::new(
            self.state.root_page.as_physical_address(),
        ));

        let frame_allocator = self.state.frame_allocator.into_mutex();

        Ok(MemoryManager::<Enabled> {
            mem_flags: self.mem_flags,
            state: Enabled {
                root_page: self.state.root_page,
                root_table,
                frame_allocator,
            },
        })
    }
}

impl MemoryManager<Enabled> {
    pub fn relocate(self) -> Result<MemoryManager<HigherHalf>, MemorySetupError> {
        let higher_half_root = self
            .state
            .frame_allocator
            .allocate_frame()
            .map(|frame| frame.page_address())
            .ok_or(MemorySetupError::OutOfMemory)?;

        let _ = Self::create_root_table(higher_half_root);

        // Настраиваем сплит верхней/нижней половины адресного пространства
        Mmu::new().enable(NormalDualSpaceConfig::new(
            self.state.root_page.as_physical_address(),
            higher_half_root.as_physical_address(),
        ));

        let ram_memory = Aarch64VirtualRamMemory::new();
        let memory_mapper = Aarch64MemoryMapper::new(
            &self.state.frame_allocator,
            self.state.root_table,
            self.mem_flags,
        );

        let _ = HeapAllocator::new(memory_mapper, ram_memory, higher_half_root.alignment());

        Ok(MemoryManager::<HigherHalf> {
            mem_flags: self.mem_flags,
            state: HigherHalf {},
        })
    }
}

impl<Any> MemoryManager<Any> {
    fn create_root_table(root: PageAlignedAddress) -> &'static mut PageTable<L0> {
        let higher_table: &'static mut PageTable<L0> =
            unsafe { &mut *(root.as_u64() as *mut PageTable<L0>) };
        *higher_table = PageTable::new();
        higher_table
    }
}

#[derive(Debug)]
pub enum MemorySetupError {
    OutOfMemory,
    NoHeapRegionFound,
}
