use crate::memory::global_allocator::{GLOBAL_ALLOCATOR, KernelHeapAllocator};
use crate::memory::layout::{MAX_MEMORY_REGIONS, MemoryLayout, MemoryRegion, RegionTag};
use crate::memory::memory_mapper::Aarch64MemoryMapper;
use crate::memory::mmu::{Mmu, NormalDualSpaceConfig};
use crate::memory::setup::HIGHER_HALF_BASE;
use aarch64_paging::level::L0;
use aarch64_paging::page_table::PageTable;
use aarch64_paging::preset::Heap;
use alloc::boxed::Box;
use collections::interval_set::IntervalSet;
use collections::{MutexCell, NoLockCell, Vec};
use klog::{debug, warn};
use memory::FrameBitmap;
use memory::RelocatablePtr;
use memory::bump_allocator::BumpAllocator;
use memory::frame::Frame;
use memory::frame_allocator::{FrameAllocator, PhysicalFrameAllocator};
use memory::memory_mapper::MemoryMapper;
use memory::memory_range::MemoryRange;
use memory::physical_address::{PageAlignedAddress, PhysicalAddress};
use memory::virtual_address::{PageAlignedVirtualAddress, VirtualAddress};

pub struct Early {
    bump_allocator: BumpAllocator,
    free_regions: IntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>,
}

/// Состояние после установки bump allocator в GLOBAL_ALLOCATOR
pub struct Installed {
    free_regions: IntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>,
}

/// Корневые таблицы страниц для lower и higher half
struct PageTableRoots {
    lower_pa: PageAlignedAddress,
    higher_pa: PageAlignedAddress,
    lower_ptr: *mut PageTable<L0>,
    higher_ptr: *mut PageTable<L0>,
}

pub struct Prepared {
    roots: PageTableRoots,
    frame_allocator: PhysicalFrameAllocator<NoLockCell<FrameBitmap>>,
    all_regions: Vec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
    bump_range: MemoryRegion<PageAlignedAddress>,
    /// Первый свободный PA для вычисления начального heap VA
    first_free_pa: PageAlignedAddress,
}

pub struct Enabled {
    frame_allocator: &'static dyn FrameAllocator,
    page_mapper: &'static dyn MemoryMapper,
    heap_start_va: PageAlignedVirtualAddress,
    higher_half_base: PageAlignedVirtualAddress,
}

pub struct MemorySetup<Stage> {
    state: Stage,
}

impl MemorySetup<Early> {
    pub fn create(layout: &MemoryLayout) -> Result<MemorySetup<Early>, MemorySetupError> {
        debug!("MemorySetup::create"; "Starting Early phase...");

        let free_regions = layout
            .free_heap_regions()
            .map_err(|_| MemorySetupError::OutOfMemory)?;

        debug!(
            "MemorySetup::create";
            "Found {} free heap regions",
            free_regions.len()
        );

        let bump_allocator = Self::create_bump_allocator(&free_regions)
            .map_err(|_| MemorySetupError::OutOfMemory)?;

        debug!("MemorySetup::create"; "Bump allocator created successfully");

        Ok(Self {
            state: Early {
                bump_allocator,
                free_regions,
            },
        })
    }

    /// Устанавливает bump allocator и возвращает Installed для цепочки вызовов
    pub fn install(self) -> MemorySetup<Installed> {
        GLOBAL_ALLOCATOR.init_bump_phase(self.state.bump_allocator);

        MemorySetup::<Installed> {
            state: Installed {
                free_regions: self.state.free_regions,
            },
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
            "create_bump_allocator";
            "Largest region: {:#x} - {:#x} (size: {} bytes)",
            largest.start.as_usize(),
            largest.end.as_usize(),
            size
        );

        Ok(BumpAllocator::new(largest.start, largest.end))
    }
}

impl MemorySetup<Installed> {
    /// Переход в Prepared фазу с учётом bump региона
    pub fn prepare(self, layout: &MemoryLayout) -> Result<MemorySetup<Prepared>, MemorySetupError> {
        // Вычитаем новые MMIO регионы из сохранённых free_regions
        let mut free_heap_regions = self.state.free_regions;

        for region in layout.iter().filter(|r| r.tag == RegionTag::Mmio) {
            debug!(
                "MemorySetup::prepare";
                "Excluding MMIO region: {:#x} - {:#x}",
                region.start.as_usize(),
                region.end.as_usize()
            );
            free_heap_regions.remove(region.start, region.end);
        }

        let first_free_pa = free_heap_regions
            .iter()
            .next()
            .map(|interval| interval.start)
            .ok_or(MemorySetupError::OutOfMemory)?;

        free_heap_regions.iter().for_each(|interval| {
            debug!(
                "MemorySetup::prepare";
                "Free heap region: {:#x} - {:#x} ({} bytes)",
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
            "MemorySetup::prepare";
            "Bump allocator used range: {:#x} - {:#x}",
            bump_start, bump_end,
        );

        let bump_range = MemoryRegion::new(RegionTag::Unknown, bump_start, bump_end, Heap::flags());

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
            "MemorySetup::prepare";
            "Allocated page table roots: lower={:#x}, higher={:#x}",
            lower_root_pa.as_usize(),
            higher_root_pa.as_usize()
        );

        let roots = PageTableRoots {
            lower_pa: lower_root_pa,
            higher_pa: higher_root_pa,
            lower_ptr: Self::create_root_table(lower_root_pa),
            higher_ptr: Self::create_root_table(higher_root_pa),
        };

        debug!("MemorySetup::prepare"; "Created root page tables");

        // Собираем все регионы
        let all_regions: Vec<_, MAX_MEMORY_REGIONS> =
            layout.iter().take(MAX_MEMORY_REGIONS).cloned().collect();

        debug!(
            "MemorySetup::prepare";
            "Collected {} memory regions",
            all_regions.len()
        );

        Ok(MemorySetup::<Prepared> {
            state: Prepared {
                roots,
                frame_allocator,
                all_regions,
                bump_range,
                first_free_pa,
            },
        })
    }
}

impl MemorySetup<Prepared> {
    pub fn enable(self) -> Result<MemorySetup<Enabled>, MemorySetupError> {
        let higher_half_base =
            PageAlignedVirtualAddress::new_unchecked(VirtualAddress::new(HIGHER_HALF_BASE));

        debug!(
            "MemorySetup::enable";
            "Higher half base: {:#x}",
            HIGHER_HALF_BASE
        );

        let heap_start_va = PageAlignedVirtualAddress::from_usize(
            HIGHER_HALF_BASE + self.state.first_free_pa.as_usize(),
        )
        .expect("heap_start_va should be page aligned");

        debug!(
            "MemorySetup::enable";
            "Heap start VA: {:#x}",
            heap_start_va.as_usize()
        );

        self.linear_map(&self.state.frame_allocator, higher_half_base)?;

        // Разбираем состояние на части
        let Prepared {
            roots,
            frame_allocator,
            ..
        } = self.state;

        debug!(
            "MemorySetup::enable";
            "Enabling MMU with TTBR0={:#x}, TTBR1={:#x}",
            roots.lower_pa.as_usize(),
            roots.higher_pa.as_usize()
        );

        // Включаем Memory Management Unit (MMU)
        Mmu::new().enable(NormalDualSpaceConfig::new(
            roots.lower_pa.as_physical_address(),
            roots.higher_pa.as_physical_address(),
        ));

        debug!("MemorySetup::enable"; "MMU enabled successfully");

        // Leak frame_allocator для 'static lifetime
        let frame_allocator: &'static PhysicalFrameAllocator<NoLockCell<FrameBitmap>> =
            Box::leak(Box::new(frame_allocator));

        debug!("MemorySetup::enable"; "Allocated frame_allocator");

        // Создаём thread-safe page_mapper для higher half с MutexCell
        let heap_flags = Heap::flags();
        let page_mapper: &'static dyn MemoryMapper = Box::leak(Box::new(
            Aarch64MemoryMapper::<_, MutexCell<_>>::new(
                frame_allocator,
                roots.higher_ptr,
                heap_flags,
            ),
        ));

        debug!("MemorySetup::enable"; "Created page_mapper for heap");

        let frame_allocator: &'static dyn FrameAllocator = frame_allocator;

        Ok(MemorySetup::<Enabled> {
            state: Enabled {
                frame_allocator,
                page_mapper,
                heap_start_va,
                higher_half_base,
            },
        })
    }

    fn linear_map<FA: FrameAllocator>(
        &self,
        frame_allocator: &FA,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> Result<(), MemorySetupError> {
        let heap_flags = Heap::flags();

        let lower_half_mapper =
            Aarch64MemoryMapper::<_, NoLockCell<_>>::new(frame_allocator, self.state.roots.lower_ptr, heap_flags);
        let higher_half_mapper =
            Aarch64MemoryMapper::<_, NoLockCell<_>>::new(frame_allocator, self.state.roots.higher_ptr, heap_flags);

        self.map_non_heap_regions(&higher_half_mapper, higher_half_base)?;
        self.map_bootstrap_identity(&lower_half_mapper)?;

        Ok(())
    }

    /// Маппит non-heap регионы (KernelText, KernelData, DeviceTree, Mmio и т.д.)
    fn map_non_heap_regions(
        &self,
        mapper: &impl MemoryMapper,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> Result<(), MemorySetupError> {
        let higher_half_base_usize = higher_half_base.as_usize();

        for region in self.state.all_regions.iter().filter(|r| !r.is_heap()) {
            let va = region.virtual_start(higher_half_base_usize);

            debug!(
                "map_non_heap_regions";
                "Mapping {:?}: PA {:#x} -> VA {:#x}, size={:#x}",
                region.tag,
                region.start.as_usize(),
                va.as_usize(),
                region.size()
            );

            mapper
                .map_exact(region.start, va, region.size(), region.flags.bits())
                .map_err(|e| {
                    warn!(
                        "map_non_heap_regions";
                        "Failed to map {:?} at PA {:#x}: {:?}",
                        region.tag,
                        region.start.as_usize(),
                        e
                    );
                    MemorySetupError::MappingFailed(e)
                })?;
        }

        Ok(())
    }

    /// Bootstrap identity mapping — kernel регионы + bump allocator range + MMIO.
    /// Нужен для выполнения кода сразу после включения MMU, до прыжка в higher half.
    fn map_bootstrap_identity(&self, mapper: &impl MemoryMapper) -> Result<(), MemorySetupError> {
        let identity_regions = self
            .state
            .all_regions
            .iter()
            .filter(|r| r.is_kernel() || r.tag == RegionTag::Mmio)
            .chain(core::iter::once(&self.state.bump_range));

        for region in identity_regions {
            debug!(
                "map_bootstrap_identity";
                "Bootstrap identity for {:?}: PA {:#x}, size={:#x}",
                region.tag,
                region.start.as_usize(),
                region.size()
            );

            let bootstrap_va = PageAlignedVirtualAddress::identity(region.start);
            mapper
                .map_exact(region.start, bootstrap_va, region.size(), region.flags.bits())
                .map_err(|e| {
                    warn!(
                        "map_bootstrap_identity";
                        "Failed to map bootstrap identity for {:?}: {:?}",
                        region.tag, e
                    );
                    MemorySetupError::MappingFailed(e)
                })?;
        }

        Ok(())
    }
}

impl MemorySetup<Enabled> {
    pub fn install(self) -> Result<Self, ()> {
        debug!("MemorySetup<Enabled>::install"; "Setting up heap allocator...");

        let allocator = KernelHeapAllocator::new(
            self.state.frame_allocator,
            self.state.page_mapper,
            self.state.heap_start_va,
            Heap::flags().bits(),
        );

        debug!(
            "MemorySetup<Enabled>::install";
            "HeapAllocator created with heap_start_va={:#x}",
            self.state.heap_start_va.as_usize()
        );

        GLOBAL_ALLOCATOR.switch_to_heap(allocator);

        // Переключаем логгер на higher half
        if let Some(writer) = klog::get_early_writer() {
            let higher_half_base = self.state.higher_half_base.as_usize();
            let new_writer = unsafe { RelocatablePtr::new(writer).relocated(higher_half_base) };
            klog::set_stdout(new_writer);
        }

        Ok(self)
    }
}

impl<Any> MemorySetup<Any> {
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

    #[allow(dead_code)]
    MappingFailed(memory::memory_mapper::MemoryMappingError),
}
