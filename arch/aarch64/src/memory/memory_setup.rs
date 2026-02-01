use crate::memory::global_allocator::{GLOBAL_ALLOCATOR, KernelHeapAllocator};
use crate::memory::layout::{MAX_MEMORY_REGIONS, MemoryLayout, MemoryRegion, RegionTag};
use crate::memory::memory_mapper::Aarch64MemoryMapper;
use crate::memory::mmu::{Mmu, NormalDualSpaceConfig};
use aarch64_paging::level::L0;
use aarch64_paging::page_table::PageTable;
use aarch64_paging::preset::Heap;
use alloc::boxed::Box;
use alloc::vec::{IntoIter, Vec};
use collections::NoLockCell;
use collections::Vec as StaticVec;
use collections::interval_set::IntervalSet;
use klog::{debug, info, warn};
use memory::FrameBitmap;
use memory::RelocatablePtr;
use memory::bump_allocator::BumpAllocator;
use memory::frame_allocator::{FrameAllocator, PhysicalFrameAllocator};
use memory::memory_mapper::MemoryMapper;
use memory::memory_range::MemoryRange;
use memory::physical_address::PageAlignedAddress;
use memory::virtual_address::PageAlignedVirtualAddress;

pub struct Early {
    bump_allocator: BumpAllocator,
    free_regions: IntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>,
    layout: MemoryLayout,
}

/// Состояние после установки bump allocator в GLOBAL_ALLOCATOR
pub struct Installed {
    free_regions: IntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>,
    layout: MemoryLayout,
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
    frame_allocator: &'static PhysicalFrameAllocator<NoLockCell<FrameBitmap>>,
    all_regions: StaticVec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
}

pub struct Enabled {
    frame_allocator: &'static dyn FrameAllocator,
    higher_half_base: PageAlignedVirtualAddress,
}

pub struct MemorySetup<Stage> {
    state: Stage,
}

impl MemorySetup<Early> {
    pub fn create(layout: MemoryLayout) -> Result<MemorySetup<Early>, MemorySetupError> {
        let free_regions = layout
            .free_heap_regions()
            .map_err(|_| MemorySetupError::OutOfMemory)?;

        let bump_allocator = Self::create_bump_allocator(&free_regions)
            .map_err(|_| MemorySetupError::OutOfMemory)?;

        Ok(Self {
            state: Early {
                layout,
                bump_allocator,
                free_regions,
            },
        })
    }

    /// Устанавливает bump allocator и возвращает Installed для цепочки вызовов
    pub fn install(self) -> MemorySetup<Installed> {
        GLOBAL_ALLOCATOR.set_bump(self.state.bump_allocator);

        MemorySetup::<Installed> {
            state: Installed {
                layout: self.state.layout,
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
            largest.start,
            largest.end,
            size
        );

        Ok(BumpAllocator::new(largest.start, largest.end))
    }
}

impl MemorySetup<Installed> {
    pub fn prepare(
        self,
        mmio: Vec<MemoryRegion<PageAlignedAddress>>,
    ) -> Result<MemorySetup<Prepared>, MemorySetupError> {
        let Installed {
            layout,
            free_regions,
        } = self.state;

        for region in layout.iter() {
            info!(
                "Memory layout";
                "- {:?}, from {:#x} to {:#x}",
                region.tag, region.start, region.end
            );
        }

        free_regions.iter().for_each(|interval| {
            debug!(
                "MemorySetup::prepare";
                "Free heap region: {:#x} - {:#x} ({} bytes)",
                interval.start, interval.end,
                interval.end.as_usize() - interval.start.as_usize()
            );
        });

        // Создаём frame_allocator из очищенных регионов
        let free_heap_regions_iter = free_regions
            .iter()
            .map(|interval| MemoryRange::new(interval.start, interval.end));

        let frame_allocator = Box::leak(Box::new(PhysicalFrameAllocator::new(
            free_heap_regions_iter,
        )));

        // Мы будем резервировать память, которая была использована bump аллокатором.
        // Замораживаем аллокацию, чтобы состояние аллокатора не изменилось
        GLOBAL_ALLOCATOR.set_freeze();

        // Исключаем только использованную часть bump
        let bump_used = GLOBAL_ALLOCATOR.get_bump().used();
        let bump_start = PageAlignedAddress::aligned_down(bump_used.start());
        let bump_end = PageAlignedAddress::aligned_up(bump_used.end());

        for region in mmio.iter() {
            frame_allocator
                .reserve_frames_exact(region.start.into(), region.end.into())
                .expect("Cannot reserve MMIO memory");
        }

        frame_allocator
            .reserve_frames_exact(bump_start.into(), bump_end.into())
            .expect("Cannot reserve bump allocator memory");

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
            "Allocated page table roots: lower={lower_root_pa:#x}, higher={higher_root_pa:#x}",
        );

        // Используем зафиксированный bump_used (заморожен в prepare)
        let bump_range = MemoryRegion::new(
            RegionTag::Other,
            bump_used.start().as_usize(),
            bump_used.end().as_usize(),
            Heap::flags(),
        );

        let all_regions = layout
            .iter()
            .cloned()
            .chain(mmio.iter().cloned())
            .chain(core::iter::once(bump_range))
            .collect::<StaticVec<_, MAX_MEMORY_REGIONS>>();

        let roots = PageTableRoots {
            lower_pa: lower_root_pa,
            higher_pa: higher_root_pa,
            lower_ptr: Self::create_root_table(lower_root_pa),
            higher_ptr: Self::create_root_table(higher_root_pa),
        };

        debug!("MemorySetup::prepare"; "Created root page tables");

        Ok(MemorySetup::<Prepared> {
            state: Prepared {
                roots,
                frame_allocator,
                all_regions,
            },
        })
    }
}

impl MemorySetup<Prepared> {
    pub fn enable(
        self,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> Result<MemorySetup<Enabled>, MemorySetupError> {
        let Prepared {
            roots,
            frame_allocator,
            all_regions,
        } = self.state;

        Self::linear_map_impl(frame_allocator, &roots, &all_regions, higher_half_base)?;

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

        let frame_allocator: &'static dyn FrameAllocator = frame_allocator;

        Ok(MemorySetup::<Enabled> {
            state: Enabled {
                frame_allocator,
                higher_half_base,
            },
        })
    }

    fn linear_map_impl<FA: FrameAllocator>(
        frame_allocator: &FA,
        roots: &PageTableRoots,
        all_regions: &StaticVec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> Result<(), MemorySetupError> {
        let heap_flags = Heap::flags();

        let lower_half_mapper = Aarch64MemoryMapper::<_, NoLockCell<_>>::new(
            frame_allocator,
            roots.lower_ptr,
            heap_flags,
        );
        let higher_half_mapper = Aarch64MemoryMapper::<_, NoLockCell<_>>::new(
            frame_allocator,
            roots.higher_ptr,
            heap_flags,
        );

        // Маппим все регионы линейно: VA = higher_half_base + PA
        Self::map_higher_half_impl(&higher_half_mapper, all_regions, higher_half_base)?;

        let identity_regions: Vec<_> = all_regions
            .iter()
            .filter(|region| !region.is_heap())
            .cloned()
            .collect();

        // Маппим регоины, к которым нужен identity доступ после включения MMU
        Self::map_identity_impl(&lower_half_mapper, identity_regions.into_iter())?;

        Ok(())
    }

    /// Маппит все регионы линейно: VA = higher_half_base + PA.
    fn map_higher_half_impl(
        mapper: &impl MemoryMapper,
        all_regions: &StaticVec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> Result<(), MemorySetupError> {
        let higher_half_base_usize = higher_half_base.as_usize();

        for region in all_regions.iter() {
            let va = region.virtual_start(higher_half_base_usize);

            debug!(
                "map_all_regions";
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
                        "map_all_regions";
                        "Failed to map {:?} at PA {:#x}: {:?}",
                        region.tag,
                        region.start,
                        e
                    );
                    MemorySetupError::MappingFailed(e)
                })?;
        }

        Ok(())
    }

    /// Identity mapping — kernel регионы + bump allocator range + MMIO.
    /// Нужен для выполнения кода сразу после включения MMU, до прыжка в higher half.
    fn map_identity_impl(
        mapper: &impl MemoryMapper,
        identity_regions: IntoIter<MemoryRegion<PageAlignedAddress>>,
    ) -> Result<(), MemorySetupError> {
        for region in identity_regions {
            debug!(
                "map_bootstrap_identity";
                "Bootstrap identity for {:?}: PA {:#x}, size={:#x}",
                region.tag, region.start, region.size()
            );

            let bootstrap_va = PageAlignedVirtualAddress::identity(region.start);
            mapper
                .map_exact(
                    region.start,
                    bootstrap_va,
                    region.size(),
                    region.flags.bits(),
                )
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
        let Enabled {
            frame_allocator,
            higher_half_base,
        } = self.state;
        let higher_half_base = higher_half_base;
        let allocator = KernelHeapAllocator::new(frame_allocator, higher_half_base);

        debug!(
            "MemorySetup<Enabled>::install";
            "Heap allocator created with higher_half_base={higher_half_base:#x}"
        );

        GLOBAL_ALLOCATOR.set_heap(allocator);

        // #region agent log [Hypothesis A]
        debug!(
            "MemorySetup<Enabled>::install";
            "set_heap done, higher_half_base={:#x}",
            self.state.higher_half_base
        );
        // #endregion

        // Переключаем логгер на higher half
        if let Some(writer) = klog::get_early_writer() {
            // #region agent log [Hypothesis A]
            // Проверяем адрес writer до релокации
            let writer_ptr = writer as *const _ as *const u8 as usize;
            debug!(
                "MemorySetup<Enabled>::install";
                "early_writer identity addr={:#x}, will relocate to {:#x}",
                writer_ptr,
                writer_ptr + higher_half_base.as_usize()
            );
            // #endregion

            let new_writer =
                unsafe { RelocatablePtr::new(writer).relocated(higher_half_base.as_virtual()) };

            // #region agent log [Hypothesis A]
            debug!(
                "MemorySetup<Enabled>::install";
                "About to call set_stdout with relocated writer"
            );
            // #endregion

            klog::set_stdout(new_writer);

            // #region agent log [Hypothesis A]
            debug!("MemorySetup<Enabled>::install"; "set_stdout completed");
            // #endregion
        }

        // #region agent log [Hypothesis A]
        debug!("MemorySetup<Enabled>::install"; "install() returning Ok");
        // #endregion

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
