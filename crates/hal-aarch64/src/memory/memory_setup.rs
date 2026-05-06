//! Поэтапная настройка памяти ядра.
//!
//! Фазы: Early -> Installed -> Prepared -> Enabled.

use alloc::boxed::Box;

use collections::{
    MutexCell, NoLockCell, Vec as StaticVec,
    interval_set::{Interval, StaticIntervalSet},
};
use hal_aarch64_paging::{
    level::L0, mapper::PageMapper, mem_flags::Aarch64MemFlags, page_table::PageTable, preset::Heap,
};
use memory::{
    FrameBitmap,
    bump_allocator::BumpAllocator,
    frame_allocator::{FrameAllocator, PhysicalFrameAllocator},
    memory_mapper::MemoryMapper,
    memory_range::MemoryRange,
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::PageAlignedVirtualAddress,
};

use crate::memory::{
    global_allocator::{GLOBAL_ALLOCATOR, KernelHeapAllocator},
    layout::{MAX_MEMORY_REGIONS, MemoryLayout, MemoryRegion, RegionTag},
    memory_mapper::{Aarch64MemoryMapper, FrameTableAlloc},
    mmu::{Mmu, NormalDualSpaceConfig},
    regs::common::EL1,
};

type MutexPageMapper<'a, FA> = MutexCell<PageMapper<FrameTableAlloc<'a, FA>>>;
type NoLockPageMapper<'a, FA> = NoLockCell<PageMapper<FrameTableAlloc<'a, FA>>>;
type FrameAllocatorImpl = PhysicalFrameAllocator<NoLockCell<FrameBitmap>>;
type NoLockAarch64MemoryMapper<'a, FA> = Aarch64MemoryMapper<'a, FA, NoLockPageMapper<'a, FA>>;

/// Ранняя фаза: bump-аллокатор создан, но не установлен.
pub struct Early {
    bump_allocator: BumpAllocator,
    free_regions: StaticIntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>,
    layout: MemoryLayout,
}

/// Фаза после установки bump-аллокатора.
pub struct Installed {
    free_regions: StaticIntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>,
    layout: MemoryLayout,
}

pub struct MemoryManagerResult {
    pub memory_mapper: Box<dyn MemoryMapper>,
    pub base_offset: PageAlignedVirtualAddress,
}

/// Корневые таблицы страниц.
pub struct PageTableRoots {
    pub lower_pa: PageAlignedAddress,
    pub higher_pa: PageAlignedAddress,
    pub lower_ptr: *mut PageTable<L0>,
    pub higher_ptr: *mut PageTable<L0>,
}

/// Фаза после подготовки таблиц страниц.
pub struct Prepared {
    /// Корневые таблицы.
    roots: PageTableRoots,
    /// Аллокатор фреймов.
    frame_allocator: &'static PhysicalFrameAllocator<NoLockCell<FrameBitmap>>,
    /// Все регионы для маппинга.
    all_regions: StaticVec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
}

/// Фаза после включения MMU.
pub struct Enabled {
    /// Корневые таблицы.
    pub roots: PageTableRoots,
    /// Аллокатор фреймов (физический адрес, был Box::leak в bump-памяти).
    pub frame_allocator: &'static FrameAllocatorImpl,
}

/// Машина состояний настройки памяти.
pub struct MemorySetup<Stage> {
    /// Текущее состояние.
    pub state: Stage,
}

impl MemorySetup<Early> {
    pub fn create(layout: MemoryLayout) -> Result<MemorySetup<Early>, MemorySetupError> {
        let free_regions = layout
            .free_heap_regions()
            .map_err(|()| MemorySetupError::OutOfMemory)?;

        // Находим самую большую свободную область
        let largest =
            Self::get_largest_region(&free_regions).map_err(|()| MemorySetupError::OutOfMemory)?;

        let bump_allocator = Self::create_bump_allocator(*largest);

        Ok(Self {
            state: Early {
                bump_allocator,
                free_regions,
                layout,
            },
        })
    }

    /// Устанавливает bump-аллокатор и переходит в Installed.
    pub fn install(self) -> MemorySetup<Installed> {
        GLOBAL_ALLOCATOR.set_bump(self.state.bump_allocator);

        MemorySetup::<Installed> {
            state: Installed {
                layout: self.state.layout,
                free_regions: self.state.free_regions,
            },
        }
    }

    fn get_largest_region(
        free_regions: &StaticIntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>,
    ) -> Result<&Interval<PageAlignedAddress>, ()> {
        free_regions
            .iter()
            .max_by_key(|interval| interval.end.as_usize() - interval.start.as_usize())
            .ok_or(())
    }

    fn create_bump_allocator(interval: Interval<PageAlignedAddress>) -> BumpAllocator {
        BumpAllocator::new(interval.start, interval.end)
    }
}

impl MemorySetup<Installed> {
    /// Подготавливает таблицы страниц (frame allocator, корневые таблицы, регионы для маппинга).
    pub fn prepare(self) -> Result<MemorySetup<Prepared>, MemorySetupError> {
        let Installed {
            layout,
            free_regions,
        } = self.state;

        // Создание frame_allocator из свободных регионов
        let free_heap_regions_iter = free_regions
            .iter()
            .map(|interval| MemoryRange::new(interval.start, interval.end));

        let frame_allocator = Box::leak(Box::new(PhysicalFrameAllocator::new(
            free_heap_regions_iter,
        )));

        // Резервирование памяти, использованной bump аллокатором.
        GLOBAL_ALLOCATOR.set_freeze();

        // SAFETY: bump инициализирован и заморожен, конкурентного доступа нет
        let bump_used = unsafe { GLOBAL_ALLOCATOR.get_bump() }.used();
        let bump_start = PageAlignedAddress::aligned_down(bump_used.start());
        let bump_end = PageAlignedAddress::aligned_up(bump_used.end());

        frame_allocator
            .reserve_frames_exact(bump_start.into(), bump_end.into())
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

        let bump_range = MemoryRegion::new(
            RegionTag::Other,
            bump_used.start().as_usize(),
            bump_used.end().as_usize(),
            Heap::flags(),
        );

        let mut mapping_heap_regions = free_regions.clone();
        mapping_heap_regions
            .remove(bump_start, bump_end)
            .ok_or(MemorySetupError::OutOfMemory)?;

        let heap_regions = mapping_heap_regions.iter().map(|interval| {
            MemoryRegion::new(
                RegionTag::Heap,
                interval.start.as_usize(),
                interval.end.as_usize(),
                Heap::flags(),
            )
        });

        let non_heap_regions = layout.iter().filter(|r| !r.is_heap()).copied();

        let mut all_regions: StaticVec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS> =
            StaticVec::new();
        for region in heap_regions
            .chain(non_heap_regions)
            .chain(core::iter::once(bump_range))
        {
            all_regions
                .push(region)
                .ok_or(MemorySetupError::OutOfMemory)?;
        }

        let roots = PageTableRoots {
            lower_pa: lower_root_pa,
            higher_pa: higher_root_pa,
            lower_ptr: Self::create_root_table(lower_root_pa),
            higher_ptr: Self::create_root_table(higher_root_pa),
        };

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
    /// Маппит все регионы и включает MMU.
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

        // Включение Memory Management Unit (MMU)
        Mmu::<EL1>::enable(&NormalDualSpaceConfig::new(
            roots.lower_pa.as_physical_address(),
            roots.higher_pa.as_physical_address(),
        ));

        Ok(MemorySetup::<Enabled> {
            state: Enabled {
                roots,
                frame_allocator,
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

        let lower_half_mapper =
            NoLockAarch64MemoryMapper::new(frame_allocator, roots.lower_ptr, heap_flags);
        let higher_half_mapper =
            NoLockAarch64MemoryMapper::new(frame_allocator, roots.higher_ptr, heap_flags);

        Self::map_higher_half_impl(&higher_half_mapper, all_regions, higher_half_base)?;

        // Identity mapping - нужен только для перехода сразу после включения MMU
        let identity_regions = all_regions
            .iter()
            .filter(|region| !region.is_heap())
            .copied();

        Self::map_identity_impl(&lower_half_mapper, identity_regions)?;

        Ok(())
    }

    fn map_higher_half_impl<FA: FrameAllocator>(
        mapper: &NoLockAarch64MemoryMapper<'_, FA>,
        all_regions: &StaticVec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> Result<(), MemorySetupError> {
        let higher_half_base_usize = higher_half_base.as_usize();

        for region in all_regions {
            let va = region.virtual_start(higher_half_base_usize);
            mapper
                .map_exact_impl(va, region.start, region.size(), region.flags)
                .map_err(MemorySetupError::MappingFailed)?;
        }

        Ok(())
    }

    fn map_identity_impl<FA: FrameAllocator>(
        mapper: &NoLockAarch64MemoryMapper<FA>,
        identity_regions: impl Iterator<Item = MemoryRegion<PageAlignedAddress>>,
    ) -> Result<(), MemorySetupError> {
        for region in identity_regions {
            let bootstrap_va = PageAlignedVirtualAddress::identity(region.start);
            mapper
                .map_exact_impl(bootstrap_va, region.start, region.size(), region.flags)
                .map_err(MemorySetupError::MappingFailed)?;
        }

        Ok(())
    }
}

impl MemorySetup<Enabled> {
    /// Переключает глобальный аллокатор на heap-фазу и возвращает маппер памяти.
    ///
    /// Вызывается post-MMU
    pub fn switch_to_heap_allocator(
        higher_root_pa: PageAlignedAddress,
        frame_allocator_phys: PhysicalAddress,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> MemoryManagerResult {
        // Аллокатор фреймов находится по физическому адресу (был Box::leak в bump-памяти).
        let fa_virt: &'static FrameAllocatorImpl =
            // SAFETY: PA + higher_half_base - корректный виртуальный адрес,
            // замапленный через TTBR1. Аллокатор был создан в bump-памяти pre-MMU.
            unsafe {
                &*((frame_allocator_phys.as_usize() + higher_half_base.as_usize())
                    as *const FrameAllocatorImpl)
            };

        // SAFETY: MMU уже включён, используется линейное отображение PA->VA (+higher_half_base),
        unsafe {
            fa_virt.relocate_inner_pointers_by_offset(higher_half_base.as_usize());
        }

        let allocator = KernelHeapAllocator::new(fa_virt, higher_half_base);
        GLOBAL_ALLOCATOR.set_heap(allocator);

        let higher_ptr_phys =
            PageAlignedVirtualAddress::identity(higher_root_pa).as_ptr::<PageTable<L0>>();
        // SAFETY: `higher_ptr_phys` - валидный указатель на PageTable<L0> в higher-half identity-region;
        // сдвиг на `higher_half_base` корректен (страница принадлежит замапленному региону).
        let higher_ptr_rel = unsafe { higher_ptr_phys.byte_add(higher_half_base.as_usize()) };
        let memory_mapper: Aarch64MemoryMapper<'_, _, MutexPageMapper<'_, _>> =
            Aarch64MemoryMapper::new_with_offset(
                fa_virt,
                higher_ptr_rel,
                Aarch64MemFlags::new(),
                higher_half_base.as_usize(),
            );

        let memory_mapper = Box::new(memory_mapper);

        Mmu::<EL1>::disable_lower_half();

        MemoryManagerResult {
            memory_mapper,
            base_offset: higher_half_base,
        }
    }
}

impl<Any> MemorySetup<Any> {
    fn create_root_table(root: PageAlignedAddress) -> *mut PageTable<L0> {
        let va = PageAlignedVirtualAddress::identity(root);
        let ptr = va.as_ptr::<PageTable<L0>>();

        // SAFETY: `root` - свежевыделенный фрейм, доступный через identity-mapping pre-MMU,
        // эксклюзивно принадлежит вызывающему; запись пустой PageTable инициализирует все entries в Invalid.
        unsafe {
            *ptr = PageTable::new();
        }

        ptr
    }
}

/// Ошибка настройки памяти.
#[derive(Debug)]
pub enum MemorySetupError {
    /// Недостаточно памяти.
    OutOfMemory,
    /// Ошибка маппинга.
    #[allow(dead_code)]
    MappingFailed(memory::memory_mapper::MemoryMappingError),
}
