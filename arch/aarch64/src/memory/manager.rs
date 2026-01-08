use crate::memory::global_allocator::{GLOBAL_ALLOCATOR, KernelHeapAllocator};
use crate::memory::layout::{MemoryLayout, MemoryRegion};
use crate::memory::memory_mapper::Aarch64MemoryMapper;
use crate::memory::mmu::{Mmu, NormalDualSpaceConfig, NormalSpaceConfig};
use aarch64_paging::level::L0;
use aarch64_paging::mem_flags::MemFlags;
use aarch64_paging::page_table::PageTable;
use alloc::boxed::Box;
use collections::{MutexCell, NoLockCell, Vec};
use core::cmp::{max, min};
use kernel::debug;
use memory::FrameBitmap;
use memory::aligned::Address;
use memory::bump_allocator::BumpAllocator;
use memory::frame::Frame;
use memory::frame_allocator::{FrameAllocator, PhysicalFrameAllocator};
use memory::memory_mapper::MemoryMapper;
use memory::memory_range::MemoryRange;
use memory::physical_address::PageAlignedAddress;
use memory::virtual_address::PageAlignedVirtualAddress;

const MAX_MEMORY_REGIONS: usize = 32;

pub struct Early {
    bump_allocator: BumpAllocator,
}

pub struct Prepared {
    root_page: PageAlignedAddress,
    root_table: *mut PageTable<L0>,
    mem_flags: MemFlags,
    frame_allocator: PhysicalFrameAllocator<NoLockCell<FrameBitmap>>,
    identity_map_regions: Vec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
}

pub struct Enabled {
    root_page: PageAlignedAddress,
    root_table: *mut PageTable<L0>,
    mem_flags: MemFlags,
    frame_allocator: &'static PhysicalFrameAllocator<MutexCell<FrameBitmap>>,
}

pub struct HigherHalf {}

/// Центральный менеджер памяти, который владеет всеми компонентами системы памяти.
pub struct MemoryManager<Stage> {
    pub(crate) state: Stage,
}

impl MemoryManager<Early> {
    pub fn create(layout: &MemoryLayout) -> Result<MemoryManager<Early>, MemorySetupError> {
        let bump_allocator = create_bump_allocator(layout)?;

        Ok(Self {
            state: Early { bump_allocator },
        })
    }

    pub fn install(self) {
        GLOBAL_ALLOCATOR.init_bump_phase(self.state.bump_allocator);
    }
}

fn create_bump_allocator(layout: &MemoryLayout) -> Result<BumpAllocator, MemorySetupError> {
    let mut free_regions: Vec<FrameInterval, MAX_REGIONS> = Vec::new();

    for heap in layout.heap() {
        let heap_start = Frame::from(heap.start).number();
        let heap_end = Frame::from(heap.end).number();

        // Собираем свободные регионы для данного heap
        collect_free_regions(&layout, &mut free_regions, heap_start, heap_end)?;
    }

    // Находим самую большую свободную область
    let largest = free_regions
        .iter()
        .max_by_key(|interval| interval.size())
        .ok_or(MemorySetupError::OutOfMemory)?;

    Ok(BumpAllocator::new(
        Frame::new(largest.start),
        Frame::new(largest.end),
    ))
}

/// Собирает свободные регионы в heap, вырезая исключённые области
fn collect_free_regions(
    layout: &MemoryLayout,
    free_regions: &mut Vec<FrameInterval, MAX_REGIONS>,
    heap_start: usize,
    heap_end: usize,
) -> Result<(), MemorySetupError> {
    // Сначала собираем и объединяем исключённые интервалы
    let mut excluded = Vec::<FrameInterval, MAX_REGIONS>::new();

    for region in layout.iter().filter(|r| r.identity_map) {
        let start = Frame::from(region.start).number();
        let end = Frame::from(region.end).number();

        let clamped_start = max(start, heap_start);
        let clamped_end = min(end, heap_end);

        if clamped_start < clamped_end {
            excluded
                .push(FrameInterval::new(clamped_start, clamped_end))
                .ok_or(MemorySetupError::StaticOutOfMemory)?;
        }
    }

    merge_intervals(&mut excluded);

    // Теперь находим свободные промежутки между исключёнными интервалами
    let mut cursor = heap_start;

    for interval in excluded.iter() {
        if cursor < interval.start {
            free_regions
                .push(FrameInterval::new(cursor, interval.start))
                .ok_or(MemorySetupError::StaticOutOfMemory)?;
        }
        cursor = max(cursor, interval.end);
    }

    // Область после последнего исключённого интервала
    if cursor < heap_end {
        let _ = free_regions.push(FrameInterval::new(cursor, heap_end));
    }

    Ok(())
}

fn merge_intervals(intervals: &mut Vec<FrameInterval, MAX_REGIONS>) {
    if intervals.is_empty() {
        return;
    }

    intervals.sort_unstable_by_key(|&interval| interval.start);

    let mut write_idx = 0;
    for i in 1..intervals.len() {
        let current = intervals[i];
        let last = intervals[write_idx];

        if current.start <= last.end {
            intervals[write_idx].end = max(last.end, current.end);
        } else {
            write_idx += 1;
            intervals[write_idx] = current;
        }
    }

    intervals.truncate(write_idx + 1);
}

const MAX_REGIONS: usize = 32;

#[derive(Clone, Copy)]
struct FrameInterval {
    start: usize,
    end: usize,
}

impl FrameInterval {
    const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    fn size(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

impl MemoryManager<Prepared> {
    pub fn create(layout: &MemoryLayout) -> Result<Self, MemorySetupError> {
        let first_heap_region = layout.heap().next();

        let heap = first_heap_region.ok_or(MemorySetupError::NoHeapRegionFound)?;
        let heap_range: MemoryRange<PageAlignedAddress> = heap.clone().into();

        let identity_map_regions: Vec<_, MAX_MEMORY_REGIONS> = layout
            .iter()
            .filter(|&region| region.identity_map)
            .map(|region| region.clone())
            .take(MAX_MEMORY_REGIONS)
            .collect();

        let identity_map_ranges: Vec<MemoryRange<PageAlignedAddress>, MAX_MEMORY_REGIONS> =
            identity_map_regions
                .iter()
                .map(|region| MemoryRange::new(region.start, region.end))
                .collect();

        let frame_allocator = PhysicalFrameAllocator::new(&heap_range, identity_map_ranges);

        let root_page = frame_allocator
            .allocate_frame()
            .map(|frame| frame.page_address())
            .ok_or(MemorySetupError::OutOfMemory)?;

        let root_table = Self::create_root_table(root_page);

        Ok(MemoryManager::<Prepared> {
            state: Prepared {
                frame_allocator,
                root_page,
                root_table,
                mem_flags: heap.flags,
                identity_map_regions,
            },
        })
    }

    pub fn enable(self) -> Result<MemoryManager<Enabled>, MemorySetupError> {
        let frame_allocator = self.state.frame_allocator.into_mutex();
        // Утекаем аллокатор в кучу, чтобы получить &'static ссылку
        let frame_allocator = Box::leak(Box::new(frame_allocator));

        let bump_allocator = GLOBAL_ALLOCATOR.take_bump_allocator();
        let arena = bump_allocator.get_used_area();

        let mut identity_regions = self.state.identity_map_regions;

        identity_regions.push(MemoryRegion::new(
            "arena",
            arena.start().as_usize(),
            arena.end().as_usize(),
            self.state.mem_flags,
            true,
        ));

        let mut mapper =
            Aarch64MemoryMapper::new(frame_allocator, self.state.root_table, self.state.mem_flags);

        for region in identity_regions.iter() {
            let range = MemoryRange::new(region.start, region.end);
            mapper
                .map_exact(
                    range.start(),
                    PageAlignedVirtualAddress::identity(range.start()),
                    range.size(),
                    region.flags.bits(),
                )
                .map_err(|_| MemorySetupError::OutOfMemory)?;
        }

        // Включаем Memory Management Unit (MMU)
        // После включения весь доступ к памяти будет через виртуальные адреса
        Mmu::new().enable(NormalSpaceConfig::new(
            self.state.root_page.as_physical_address(),
        ));

        Ok(MemoryManager::<Enabled> {
            state: Enabled {
                mem_flags: self.state.mem_flags,
                root_table: self.state.root_table,
                root_page: self.state.root_page,
                frame_allocator,
            },
        })
    }
}

impl MemoryManager<Enabled> {
    pub fn install(self) -> Result<Self, ()> {
        let frame_allocator = self.state.frame_allocator;
        let heap_range = frame_allocator.heap_range();

        let mapper =
            Aarch64MemoryMapper::new(frame_allocator, self.state.root_table, self.state.mem_flags);

        let mut allocator = KernelHeapAllocator::new(mapper, heap_range);

        allocator.init().map_err(|_| ())?;
        debug!("Allocator initialized!");

        GLOBAL_ALLOCATOR.switch_to_heap(allocator);

        Ok(self)
    }

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

        Ok(MemoryManager::<HigherHalf> {
            state: HigherHalf {},
        })
    }
}

impl<Any> MemoryManager<Any> {
    fn create_root_table(root: PageAlignedAddress) -> *mut PageTable<L0> {
        let va = PageAlignedVirtualAddress::identity(root);
        let ptr = va.as_ptr::<PageTable<L0>>();

        unsafe {
            core::ptr::write(ptr, PageTable::new());
        };

        ptr
    }
}

#[derive(Debug)]
pub enum MemorySetupError {
    OutOfMemory,
    StaticOutOfMemory,
    NoHeapRegionFound,
}
