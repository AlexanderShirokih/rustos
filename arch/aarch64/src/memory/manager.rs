use crate::memory::global_allocator::{GLOBAL_ALLOCATOR, KernelHeapAllocator};
use crate::memory::layout::{MemoryLayout, MemoryRegion};
use crate::memory::memory_mapper::Aarch64MemoryMapper;
use crate::memory::mmu::{Mmu, NormalDualSpaceConfig};
use crate::memory::setup::HIGHER_HALF_BASE;
use aarch64_paging::level::L0;
use aarch64_paging::page_table::PageTable;
use aarch64_paging::preset::Heap;
use alloc::boxed::Box;
use collections::interval_set::IntervalSet;
use collections::{NoLockCell, Vec};
use klog::debug;
use memory::FrameBitmap;
use memory::bump_allocator::BumpAllocator;
use memory::frame::Frame;
use memory::frame_allocator::{FrameAllocator, PhysicalFrameAllocator};
use memory::memory_mapper::MemoryMapper;
use memory::memory_range::MemoryRange;
use memory::physical_address::{PageAlignedAddress, PhysicalAddress};
use memory::region_manager::RegionManager;
use memory::virtual_address::{PageAlignedVirtualAddress, VirtualAddress};

const MAX_MEMORY_REGIONS: usize = 24;

pub struct Early {
    bump_allocator: BumpAllocator,
}

/// Состояние после установки bump allocator в GLOBAL_ALLOCATOR
pub struct Installed {}

pub struct Prepared {
    lower_root_pa: PageAlignedAddress,
    higher_root_pa: PageAlignedAddress,
    lower_root_table: *mut PageTable<L0>,
    higher_root_table: *mut PageTable<L0>,
    frame_allocator: PhysicalFrameAllocator<NoLockCell<FrameBitmap>>,
    all_regions: Vec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
    free_heap_regions: IntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>,
}

pub struct Enabled {
    pub higher_half_base: PageAlignedVirtualAddress,
    free_heap_regions: IntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>,
}

pub struct MemoryManager<Stage> {
    state: Stage,
}

impl MemoryManager<Early> {
    pub fn create(layout: &MemoryLayout) -> Result<MemoryManager<Early>, MemorySetupError> {
        let free_regions =
            get_free_heap_regions(layout).map_err(|_| MemorySetupError::OutOfMemory)?;
        let bump_allocator = Self::create_bump_allocator(&free_regions)
            .map_err(|_| MemorySetupError::OutOfMemory)?;

        Ok(Self {
            state: Early { bump_allocator },
        })
    }

    /// Устанавливает bump allocator и возвращает Installed для цепочки вызовов
    pub fn install(self) -> MemoryManager<Installed> {
        GLOBAL_ALLOCATOR.init_bump_phase(self.state.bump_allocator);

        MemoryManager::<Installed> {
            state: Installed {},
        }
    }

    fn create_bump_allocator(
        free_regions: &IntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>,
    ) -> Result<BumpAllocator, ()> {
        // Находим самую большую свободную область
        let largest = free_regions
            .iter()
            .max_by_key(|interval| interval.end.as_usize() - interval.start.as_usize())
            .ok_or(())?;

        Ok(BumpAllocator::new(largest.start, largest.end))
    }
}

impl MemoryManager<Installed> {
    /// Переход в Prepared фазу с учётом bump региона
    pub fn prepare(self, layout: &MemoryLayout) -> Result<MemoryManager<Prepared>, MemorySetupError> {
        // Пересчитываем free regions с учётом новых MMIO
        let free_heap_regions =
            get_free_heap_regions(layout).map_err(|_| MemorySetupError::OutOfMemory)?;

        let free_heap_regions_iter = free_heap_regions
            .iter()
            .map(|interval| MemoryRange::new(interval.start, interval.end));

        let frame_allocator = PhysicalFrameAllocator::new(free_heap_regions_iter);

        // Резервируем фактически использованную область bump allocator'а
        let (bump_start, bump_end) = GLOBAL_ALLOCATOR.bump_used_range();
        let start_frame = Frame::from(PageAlignedAddress::aligned_down(
            PhysicalAddress::new(bump_start),
        ));
        let end_frame = Frame::from(PageAlignedAddress::aligned_up(
            PhysicalAddress::new(bump_end),
        ));

        frame_allocator
            .reserve_frames_exact(start_frame, end_frame)
            .map_err(|_| MemorySetupError::OutOfMemory)?;

        // Выделяем страницы под корень таблиц страниц
        let lower_root_pa = frame_allocator
            .allocate_frame()
            .map(|frame| frame.page_address())
            .ok_or(MemorySetupError::OutOfMemory)?;

        let higher_root_pa = frame_allocator
            .allocate_frame()
            .map(|frame| frame.page_address())
            .ok_or(MemorySetupError::OutOfMemory)?;

        let lower_root_table = Self::create_root_table(lower_root_pa);
        let higher_root_table = Self::create_root_table(higher_root_pa);

        // Собираем все регионы
        let all_regions: Vec<_, MAX_MEMORY_REGIONS> =
            layout.iter().take(MAX_MEMORY_REGIONS).cloned().collect();

        Ok(MemoryManager::<Prepared> {
            state: Prepared {
                frame_allocator,
                lower_root_pa,
                higher_root_pa,
                lower_root_table,
                higher_root_table,
                all_regions,
                free_heap_regions,
            },
        })
    }
}

impl MemoryManager<Prepared> {

    pub fn enable(self) -> Result<MemoryManager<Enabled>, MemorySetupError> {
        let higher_half_base =
            PageAlignedVirtualAddress::new_unchecked(VirtualAddress::new(HIGHER_HALF_BASE));

        // Отображаем все регионы в higher half, bootstrap identity для kernel code
        self.linear_map(&self.state.frame_allocator, higher_half_base)?;

        // После маппинга можем безопасно разбирать состояние на части.
        let Prepared {
            frame_allocator,
            lower_root_pa,
            higher_root_pa,
            free_heap_regions,
            ..
        } = self.state;

        let frame_allocator = frame_allocator.into_mutex();
        // Утекаем аллокатор в кучу, чтобы получить &'static ссылку
        let _frame_allocator: &'static _ = Box::leak(Box::new(frame_allocator));

        // Включаем Memory Management Unit (MMU)
        Mmu::new().enable(NormalDualSpaceConfig::new(
            lower_root_pa.as_physical_address(),
            higher_root_pa.as_physical_address(),
        ));

        Ok(MemoryManager::<Enabled> {
            state: Enabled {
                higher_half_base,
                free_heap_regions,
            },
        })
    }

    fn linear_map<FA: FrameAllocator>(
        &self,
        frame_allocator: &FA,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> Result<(), MemorySetupError> {
        let heap_flags = Heap::flags();
        let higher_half_base_usize = higher_half_base.as_usize();

        let mut lower_half_mapper =
            Aarch64MemoryMapper::new(frame_allocator, self.state.lower_root_table, heap_flags);

        let mut higher_half_mapper =
            Aarch64MemoryMapper::new(frame_allocator, self.state.higher_root_table, heap_flags);

        // 1. Маппим ВСЕ регионы в higher half
        for region in self.state.all_regions.iter() {
            let va = region.virtual_start(higher_half_base_usize);

            higher_half_mapper
                .map_exact(region.start, va, region.size(), region.flags.bits())
                .map_err(|_| MemorySetupError::OutOfMemory)?;
        }

        // 2. Bootstrap identity mapping — только kernel code
        //    Нужен для выполнения кода сразу после включения MMU, до прыжка в higher half
        let kernel_code = self
            .state
            .all_regions
            .iter()
            .find(|r| r.is_kernel_code())
            .ok_or(MemorySetupError::OutOfMemory)?;

        let bootstrap_va = PageAlignedVirtualAddress::identity(kernel_code.start);
        lower_half_mapper
            .map_exact(
                kernel_code.start,
                bootstrap_va,
                kernel_code.size(),
                kernel_code.flags.bits(),
            )
            .map_err(|_| MemorySetupError::OutOfMemory)?;

        Ok(())
    }
}

impl MemoryManager<Enabled> {
    pub fn install(self) -> Result<Self, ()> {
        // Все регионы теперь в higher half
        let higher_half_base = self.state.higher_half_base.as_usize();

        let free_regions: Vec<MemoryRange<VirtualAddress>, MAX_MEMORY_REGIONS> = self
            .state
            .free_heap_regions
            .iter()
            .map(|interval| {
                MemoryRange::new(
                    VirtualAddress::new(interval.start.as_usize() + higher_half_base),
                    VirtualAddress::new(interval.end.as_usize() + higher_half_base),
                )
            })
            .collect();

        let region_manager = RegionManager::new(free_regions);

        // Утекаем RegionManager чтобы получить &'static
        let region_manager: &'static RegionManager = Box::leak(Box::new(region_manager));

        // Создаём HeapAllocator
        let allocator =
            KernelHeapAllocator::new(region_manager, self.state.higher_half_base.into());

        debug!("Heap allocator initialized!");

        GLOBAL_ALLOCATOR.switch_to_heap(allocator);

        Ok(self)
    }
}

fn get_free_heap_regions(
    layout: &MemoryLayout,
) -> Result<IntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>, ()> {
    let mut free_regions = IntervalSet::<PageAlignedAddress, MAX_MEMORY_REGIONS>::new();

    // Добавляем свободные области (heap-регионы)
    let heap_regions = layout.iter().filter(|region| region.is_heap());

    for heap in heap_regions {
        let result = free_regions.add(heap.start, heap.end);

        if result.is_none() {
            return Err(());
        }
    }

    // Вычитаем занятые (зарезервированные) области
    let non_heap_regions = layout.iter().filter(|region| !region.is_heap());
    for region in non_heap_regions {
        let result = free_regions.remove(region.start, region.end);

        if result.is_none() {
            return Err(());
        }
    }

    Ok(free_regions)
}

impl<Any> MemoryManager<Any> {
    fn create_root_table(root: PageAlignedAddress) -> *mut PageTable<L0> {
        let va = PageAlignedVirtualAddress::identity(root);
        let ptr = va.as_ptr::<PageTable<L0>>();

        unsafe {
            *ptr = PageTable::new();
        };

        ptr
    }
}

#[derive(Debug)]
pub enum MemorySetupError {
    OutOfMemory,
}
