use crate::memory::global_allocator::{GLOBAL_ALLOCATOR, KernelHeapAllocator};
use crate::memory::layout::{MemoryLayout, MemoryRegion, RegionTag};
use crate::memory::memory_mapper::Aarch64MemoryMapper;
use crate::memory::mmu::{Mmu, NormalDualSpaceConfig};
use crate::memory::setup::HIGHER_HALF_BASE;
use aarch64_paging::level::L0;
use aarch64_paging::page_table::PageTable;
use aarch64_paging::preset::Heap;
use alloc::boxed::Box;
use collections::interval_set::IntervalSet;
use collections::{NoLockCell, Vec};
use klog::{debug, warn};
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
    bump_range: MemoryRegion<PageAlignedAddress>,
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
        debug!("[MemoryManager::create] Starting Early phase...");

        let free_regions =
            get_free_heap_regions(layout).map_err(|_| MemorySetupError::OutOfMemory)?;

        debug!(
            "[MemoryManager::create] Found {} free heap regions",
            free_regions.len()
        );

        let bump_allocator = Self::create_bump_allocator(&free_regions)
            .map_err(|_| MemorySetupError::OutOfMemory)?;

        debug!("[MemoryManager::create] Bump allocator created successfully");

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

        let size = largest.end.as_usize() - largest.start.as_usize();
        debug!(
            "[create_bump_allocator] Largest region: {:#x} - {:#x} (size: {} bytes)",
            largest.start.as_usize(),
            largest.end.as_usize(),
            size
        );

        Ok(BumpAllocator::new(largest.start, largest.end))
    }
}

impl MemoryManager<Installed> {
    /// Переход в Prepared фазу с учётом bump региона
    pub fn prepare(
        self,
        layout: &MemoryLayout,
    ) -> Result<MemoryManager<Prepared>, MemorySetupError> {
        debug!("[MemoryManager::prepare] Starting Prepared phase...");

        // Пересчитываем free regions с учётом новых MMIO
        let free_heap_regions =
            get_free_heap_regions(layout).map_err(|_| MemorySetupError::OutOfMemory)?;

        free_heap_regions.iter().for_each(|interval| {
            debug!(
                "[MemoryManager::prepare] Recalculated free heap region: {:#x} - {:#x} ({} bytes)",
                interval.start.as_usize(),
                interval.end.as_usize(),
                interval.end.as_usize() - interval.start.as_usize()
            );
        });

        let free_heap_regions_iter = free_heap_regions
            .iter()
            .map(|interval| MemoryRange::new(interval.start, interval.end));

        let frame_allocator = PhysicalFrameAllocator::new(free_heap_regions_iter);

        // Резервируем фактически использованную область bump allocator'а
        let (bump_start, bump_end) = GLOBAL_ALLOCATOR.bump_used_range();
        debug!(
            "[MemoryManager::prepare] Bump allocator used range: {:#x} - {:#x}",
            bump_start, bump_end,
        );

        let bump_range = MemoryRegion::new(
            RegionTag::Unknown,
            bump_start,
            bump_end,
            Heap::flags(),
        );

        let bump_start_frame = Frame::from(PageAlignedAddress::aligned_down(PhysicalAddress::new(
            bump_start,
        )));
        let bump_end_frame = Frame::from(PageAlignedAddress::aligned_up(PhysicalAddress::new(
            bump_end,
        )));

        frame_allocator
            .reserve_frames_exact(bump_start_frame, bump_end_frame)
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

        debug!(
            "[MemoryManager::prepare] Allocated page table roots: lower={:#x}, higher={:#x}",
            lower_root_pa.as_usize(),
            higher_root_pa.as_usize()
        );

        let lower_root_table = Self::create_root_table(lower_root_pa);
        let higher_root_table = Self::create_root_table(higher_root_pa);

        debug!("[MemoryManager::prepare] Created root page tables");

        // Собираем все регионы
        let all_regions: Vec<_, MAX_MEMORY_REGIONS> =
            layout.iter().take(MAX_MEMORY_REGIONS).cloned().collect();

        debug!(
            "[MemoryManager::prepare] Collected {} memory regions",
            all_regions.len()
        );

        Ok(MemoryManager::<Prepared> {
            state: Prepared {
                frame_allocator,
                lower_root_pa,
                higher_root_pa,
                lower_root_table,
                higher_root_table,
                all_regions,
                free_heap_regions,
                bump_range,
            },
        })
    }
}

impl MemoryManager<Prepared> {
    pub fn enable(self) -> Result<MemoryManager<Enabled>, MemorySetupError> {
        debug!("[MemoryManager::enable] Starting Enabled phase...");

        let higher_half_base =
            PageAlignedVirtualAddress::new_unchecked(VirtualAddress::new(HIGHER_HALF_BASE));

        debug!(
            "[MemoryManager::enable] Higher half base: {:#x}",
            HIGHER_HALF_BASE
        );

        // Отображаем все регионы в higher half, bootstrap identity для kernel code
        debug!("[MemoryManager::enable] Setting up linear mapping...");
        self.linear_map(&self.state.frame_allocator, higher_half_base)?;
        debug!("[MemoryManager::enable] Linear mapping complete");

        // После маппинга можем безопасно разбирать состояние на части.
        let Prepared {
            lower_root_pa,
            higher_root_pa,
            free_heap_regions,
            ..
        } = self.state;

        debug!(
            "[MemoryManager::enable] Enabling MMU with TTBR0={:#x}, TTBR1={:#x}",
            lower_root_pa.as_usize(),
            higher_root_pa.as_usize()
        );

        // Включаем Memory Management Unit (MMU)
        Mmu::new().enable(NormalDualSpaceConfig::new(
            lower_root_pa.as_physical_address(),
            higher_root_pa.as_physical_address(),
        ));

        debug!("[MemoryManager::enable] MMU enabled successfully!");

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

        // 1. Маппим non-heap регионы (KernelText, KernelData, DeviceTree, Mmio и т.д.)
        //    Heap не маппим целиком, т.к. он перекрывает вложенные регионы
        let all_regions = &self.state.all_regions;
        let non_heap_regions = all_regions.iter().filter(|r| !r.is_heap());
        let non_heap_count = all_regions.iter().filter(|r| !r.is_heap()).count();

        debug!(
            "[linear_map] Mapping {} non-heap regions to higher half",
            non_heap_count
        );
        for region in non_heap_regions {
            let va = region.virtual_start(higher_half_base_usize);

            debug!(
                "[linear_map] Mapping region {:?}: PA {:#x} -> VA {:#x}, size={:#x}, flags={:#x}",
                region.tag,
                region.start.as_usize(),
                va.as_usize(),
                region.size(),
                region.flags.bits()
            );

            higher_half_mapper
                .map_exact(region.start, va, region.size(), region.flags.bits())
                .map_err(|e| {
                    warn!(
                        "[linear_map] Failed to map region {:?} at PA {:#x}: {:?}",
                        region.tag,
                        region.start.as_usize(),
                        e
                    );
                    MemorySetupError::MappingFailed(e)
                })?;
        }

        // 2. Маппим свободные части Heap (free_heap_regions)
        debug!(
            "[linear_map] Mapping {} free heap regions",
            self.state.free_heap_regions.len()
        );
        for interval in self.state.free_heap_regions.iter() {
            let pa = interval.start;
            let size = interval.end.as_usize() - interval.start.as_usize();
            let va = PageAlignedVirtualAddress::from_usize(higher_half_base_usize + pa.as_usize())
                .expect("address should be page aligned");

            debug!(
                "[linear_map] Mapping free heap: PA {:#x} -> VA {:#x}, size={:#x}",
                pa.as_usize(),
                va.as_usize(),
                size
            );

            higher_half_mapper
                .map_exact(pa, va, size, heap_flags.bits())
                .map_err(|e| {
                    warn!(
                        "[linear_map] Failed to map free heap at PA {:#x}: {:?}",
                        pa.as_usize(),
                        e
                    );
                    MemorySetupError::MappingFailed(e)
                })?;
        }

        // 2. Bootstrap identity mapping — kernel регионы + bump allocator range + MMIO
        //    Нужен для выполнения кода сразу после включения MMU, до прыжка в higher half.
        //    MMIO нужен для вывода отладки между включением MMU и прыжком в higher half.
        let identity_regions = self
            .state
            .all_regions
            .iter()
            .filter(|r| r.is_kernel() || r.tag == RegionTag::Mmio)
            .chain(core::iter::once(&self.state.bump_range));

        for region in identity_regions {
            debug!(
                "[linear_map] Bootstrap identity mapping for {:?}: PA {:#x}, size={:#x}",
                region.tag,
                region.start.as_usize(),
                region.size()
            );

            let bootstrap_va = PageAlignedVirtualAddress::identity(region.start);
            lower_half_mapper
                .map_exact(region.start, bootstrap_va, region.size(), region.flags.bits())
                .map_err(|e| {
                    warn!(
                        "[linear_map] Failed to map bootstrap identity for {:?}: {:?}",
                        region.tag, e
                    );
                    MemorySetupError::MappingFailed(e)
                })?;
        }

        Ok(())
    }
}

impl MemoryManager<Enabled> {
    pub fn install(self) -> Result<Self, ()> {
        debug!("[MemoryManager<Enabled>::install] Setting up heap allocator...");

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

        debug!(
            "[MemoryManager<Enabled>::install] Converted {} free regions to virtual addresses",
            free_regions.len()
        );

        for region in free_regions.iter() {
            debug!(
                "[MemoryManager<Enabled>::install] Free heap VA region: {:#x} - {:#x} ({} bytes)",
                region.start().as_usize(),
                region.end().as_usize(),
                region.end().as_usize() - region.start().as_usize()
            );
        }

        let region_manager = RegionManager::new(free_regions);

        // Утекаем RegionManager чтобы получить &'static
        let region_manager: &'static RegionManager = Box::leak(Box::new(region_manager));

        // Создаём HeapAllocator
        let allocator =
            KernelHeapAllocator::new(region_manager, self.state.higher_half_base.into());

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
            warn!("[get_free_heap_regions] ERROR: Failed to add heap region");
            return Err(());
        }
    }

    // Вычитаем занятые (зарезервированные) области
    let non_heap_regions = layout.iter().filter(|region| !region.is_heap());
    for region in non_heap_regions {
        let result = free_regions.remove(region.start, region.end);

        if result.is_none() {
            warn!("[get_free_heap_regions] ERROR: Failed to remove reserved region");
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
    MappingFailed(memory::memory_mapper::MemoryMappingError),
}
