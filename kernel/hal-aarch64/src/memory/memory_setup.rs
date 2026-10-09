//! Поэтапная настройка памяти ядра.
//!
//! Фазы: Early -> Installed -> Prepared -> Enabled.

use alloc::boxed::Box;

use hal_aarch64_paging::{level::L0, mapper::PageMapper, page_table::PageTable, preset::Heap};
use io::Vec as StaticVec;
use kbase::{
    MutexCell, NoLockCell,
    interval_set::{Interval, StaticIntervalSet},
};
use memory::{
    FrameBitmap,
    bump_allocator::BumpAllocator,
    frame_allocator::{FrameAllocator, FrameCount, PhysicalFrameAllocator},
    kernel_vm_allocator::{HeapAllocator, HeapArena},
    memory_mapper::{AddressSpaceFactory, MemoryMapper},
    memory_range::MemoryRange,
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::PageAlignedVirtualAddress,
};
use spin::Once;

use crate::{
    consts::{KHEAP_BASE, KHEAP_MAX_SIZE},
    memory::{
        address_space_factory::FrameAllocatorImpl,
        asid,
        global_allocator::GLOBAL_ALLOCATOR,
        layout::{MAX_MEMORY_REGIONS, MemoryLayout, MemoryRegion, RegionTag, TooManyMemoryRegions},
        memory_mapper::{Aarch64MemoryMapper, AddressSpaceKind, FrameTableAlloc},
        mmu::{Mmu, NormalDualSpaceConfig},
        regs::{common::EL1, dcache::DataCache, id_aa64mmfr0::IdAa64Mmfr0},
    },
};

pub(crate) type BootFrameAllocator = PhysicalFrameAllocator<NoLockCell<FrameBitmap>>;

type MutexPageMapper<'a, FA> = MutexCell<PageMapper<FrameTableAlloc<'a, FA>>>;
type NoLockPageMapper<'a, FA> = NoLockCell<PageMapper<FrameTableAlloc<'a, FA>>>;
type NoLockAarch64MemoryMapper<'a, FA> = Aarch64MemoryMapper<'a, FA, NoLockPageMapper<'a, FA>>;
type KernelMemoryMapper =
    Aarch64MemoryMapper<'static, FrameAllocatorImpl, MutexPageMapper<'static, FrameAllocatorImpl>>;

static KERNEL_MEMORY_MAPPER: Once<KernelMemoryMapper> = Once::new();

static FRAME_ALLOCATOR: Once<FrameAllocatorImpl> = Once::new();

#[cfg(feature = "kernel-tests")]
pub(crate) fn frame_allocator() -> Option<&'static FrameAllocatorImpl> {
    FRAME_ALLOCATOR.get()
}

pub(crate) struct Early {
    bump_allocator: BumpAllocator,
    free_regions: StaticIntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>,
    layout: MemoryLayout,
}

pub(crate) struct Installed {
    free_regions: StaticIntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>,
    layout: MemoryLayout,
}

pub(crate) struct MemoryManagerResult {
    pub memory_mapper: &'static (dyn MemoryMapper + Send + Sync),
    pub address_space_factory: Box<dyn AddressSpaceFactory + Send + Sync>,
    pub frame_allocator: &'static (dyn FrameAllocator + Send + Sync),
    /// Тот же аллокатор фреймов как источник счёта фреймов.
    pub frame_count: &'static (dyn FrameCount + Send + Sync),
}

pub(crate) struct PageTableRoots {
    pub lower_pa: PageAlignedAddress,
    pub higher_pa: PageAlignedAddress,
    pub lower_ptr: *mut PageTable<L0>,
    pub higher_ptr: *mut PageTable<L0>,
}

pub(crate) struct Prepared {
    roots: PageTableRoots,
    frame_allocator: &'static BootFrameAllocator,
    all_regions: StaticVec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
    bump: Interval<PageAlignedAddress>,
}

pub(crate) struct Enabled {
    /// Корневые таблицы.
    pub roots: PageTableRoots,
    /// Аллокатор фреймов (физический адрес, был Box::leak в bump-памяти).
    pub frame_allocator: &'static BootFrameAllocator,
}

pub(crate) struct MemorySetup<Stage> {
    /// Текущее состояние.
    pub state: Stage,
}

impl MemorySetup<Early> {
    pub(crate) fn create(layout: MemoryLayout) -> Result<MemorySetup<Early>, MemorySetupError> {
        let free_regions = layout
            .free_heap_regions()
            .map_err(|()| MemorySetupError::OutOfMemory)?;

        // Находим самую большую свободную область
        // Свободная RAM ещё ничего не содержит, поэтому сброс строк идёт до первой
        // записи, чтобы оставленная загрузчиком грязная строка не вытеснилась
        // поверх неё позднее.
        for interval in free_regions.iter() {
            // SAFETY: MMU ещё выключен. Свободная RAM принадлежит ядру, и ядро в неё не писало.
            unsafe {
                DataCache::<EL1>::invalidate_range_to_coherency(
                    interval.start.as_usize(),
                    interval.end.as_usize(),
                );
            }
        }

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

    pub(crate) fn install(self) -> MemorySetup<Installed> {
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
    pub(crate) fn prepare(self) -> Result<MemorySetup<Prepared>, MemorySetupError> {
        let Installed {
            layout,
            free_regions,
        } = self.state;

        let free_heap_regions_iter = free_regions
            .iter()
            .map(|interval| MemoryRange::new(interval.start, interval.end));

        let frame_allocator = Box::leak(Box::new(
            PhysicalFrameAllocator::new(free_heap_regions_iter)
                .map_err(MemorySetupError::FrameAllocatorInit)?,
        ));

        // Резервирование памяти, использованной bump аллокатором.
        GLOBAL_ALLOCATOR.set_freeze();

        // SAFETY: bump инициализирован и заморожен, конкурентный доступ отсутствует
        let bump_used = unsafe { GLOBAL_ALLOCATOR.with_bump(|bump| bump.used()) };
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

        let mut all_regions: StaticVec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS> =
            StaticVec::new();
        for region in heap_regions {
            all_regions
                .push(region)
                .ok_or(MemorySetupError::OutOfMemory)?;
        }

        layout
            .push_mapped_non_heap_regions(&mut all_regions)
            .map_err(|TooManyMemoryRegions| MemorySetupError::OutOfMemory)?;

        all_regions
            .push(bump_range)
            .ok_or(MemorySetupError::OutOfMemory)?;

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
                bump: Interval {
                    start: bump_start,
                    end: bump_end,
                },
            },
        })
    }
}

impl MemorySetup<Prepared> {
    pub(crate) fn enable(
        self,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> Result<MemorySetup<Enabled>, MemorySetupError> {
        let Prepared {
            roots,
            frame_allocator,
            all_regions,
            bump,
        } = self.state;
        Self::linear_map_impl(frame_allocator, &roots, &all_regions, higher_half_base)?;

        Self::drop_stale_cache_lines(&all_regions, bump);

        // Включение Memory Management Unit (MMU)
        Mmu::<EL1>::enable(&NormalDualSpaceConfig::new(
            roots.lower_pa.as_physical_address(),
            roots.higher_pa.as_physical_address(),
        ));

        // Аллокатор ASID один на систему, и настраивает его только boot CPU;
        // через `Mmu::enable` проходит каждый. Настройка идёт после включения
        // MMU: до него запись легла бы мимо кэша.
        asid::init_global(IdAa64Mmfr0::asid_width());

        Ok(MemorySetup::<Enabled> {
            state: Enabled {
                roots,
                frame_allocator,
            },
        })
    }

    fn drop_stale_cache_lines(
        all_regions: &StaticVec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
        bump: Interval<PageAlignedAddress>,
    ) {
        // Парный открывающий проход идёт в `MemorySetup::<Early>::create`, до
        // первой записи. Bump-арены нет среди `all_regions` под тегом
        // [`RegionTag::Heap`], поэтому она инвалидируется отдельно.
        // SAFETY: MMU включается после этого вызова, и записи ядра идут мимо кэша. Bump-арена
        // принадлежит ядру, её грязные строки оставлены загрузчиком.
        unsafe {
            DataCache::<EL1>::invalidate_range_to_coherency(
                bump.start.as_usize(),
                bump.end.as_usize(),
            );
        }

        for region in all_regions
            .iter()
            .filter(|region| region.tag == RegionTag::Heap)
        {
            // SAFETY: как для bump-арены выше: MMU выключен, Heap-регион принадлежит ядру.
            unsafe {
                DataCache::<EL1>::invalidate_range_to_coherency(
                    region.start.as_usize(),
                    region.end.as_usize(),
                );
            }
        }
    }

    fn linear_map_impl<FA: FrameAllocator>(
        frame_allocator: &FA,
        roots: &PageTableRoots,
        all_regions: &StaticVec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> Result<(), MemorySetupError> {
        let lower_half_mapper = NoLockAarch64MemoryMapper::new_with_offset(
            frame_allocator,
            roots.lower_ptr,
            0,
            AddressSpaceKind::Kernel,
        );
        let higher_half_mapper = NoLockAarch64MemoryMapper::new_with_offset(
            frame_allocator,
            roots.higher_ptr,
            0,
            AddressSpaceKind::Kernel,
        );

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
    pub(crate) fn switch_to_heap_allocator(
        higher_root_pa: PageAlignedAddress,
        frame_allocator_phys: PhysicalAddress,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> MemoryManagerResult {
        let fa_virt = Self::adopt_frame_allocator(frame_allocator_phys, higher_half_base);

        let higher_ptr_phys =
            PageAlignedVirtualAddress::identity(higher_root_pa).as_ptr::<PageTable<L0>>();
        // SAFETY: `higher_ptr_phys` - валидный указатель на PageTable<L0> в higher-half
        // identity-region. Сдвиг на `higher_half_base` корректен, потому что страница принадлежит
        // замапленному региону.
        let higher_ptr_rel = unsafe { higher_ptr_phys.byte_add(higher_half_base.as_usize()) };

        // Слот статический: `set_heap` идёт при global allocator в PHASE_FROZEN,
        // и положить mapper в Box в этот момент уже нельзя.
        let kernel_mapper: &'static KernelMemoryMapper = KERNEL_MEMORY_MAPPER.call_once(|| {
            Aarch64MemoryMapper::new_with_offset(
                fa_virt,
                higher_ptr_rel,
                higher_half_base.as_usize(),
                AddressSpaceKind::Kernel,
            )
        });

        let allocator = HeapAllocator::new(
            kernel_mapper as &'static (dyn MemoryMapper + Send + Sync),
            HeapArena::new(KHEAP_BASE, KHEAP_MAX_SIZE),
        );
        GLOBAL_ALLOCATOR.set_heap(allocator);

        let address_space_factory = Box::new(
            crate::memory::address_space_factory::Aarch64AddressSpaceFactory::new(
                fa_virt,
                higher_half_base,
            ),
        );

        // SAFETY: `primary_main` получает управление после прыжка в higher half. DAIF замаскирован
        // со входа в ядро и до этой точки не снимается.
        unsafe { Mmu::<EL1>::disable_lower_half() };

        MemoryManagerResult {
            memory_mapper: kernel_mapper,
            address_space_factory,
            frame_allocator: fa_virt,
            frame_count: fa_virt,
        }
    }

    fn adopt_frame_allocator(
        frame_allocator_phys: PhysicalAddress,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> &'static FrameAllocatorImpl {
        let boot_ptr = (frame_allocator_phys.as_usize() + higher_half_base.as_usize())
            as *const BootFrameAllocator;

        // SAFETY: `frame_allocator_phys` указывает на `Box::leak`-нутый в
        // bump-памяти аллокатор, а PA + higher_half_base - его виртуальный
        // адрес в линейном отображении TTBR1. Чтение забирает владение: bump-
        // память под ним зарезервирована и больше никем не читается, а сам
        // физический адрес дальше по загрузке не передаётся.
        let boot = unsafe { boot_ptr.read() };

        // SAFETY: MMU включён, отображение PA->VA линейно со сдвигом
        // `higher_half_base`; вызов идёт до первого обращения к аллокатору.
        unsafe {
            boot.relocate_inner_pointers_by_offset(higher_half_base.as_usize());
        }

        // Аллокатор размещён в статике. Куча ядра строится поверх него.
        FRAME_ALLOCATOR.call_once(|| boot.into_locked())
    }
}

impl<Any> MemorySetup<Any> {
    fn create_root_table(root: PageAlignedAddress) -> *mut PageTable<L0> {
        let va = PageAlignedVirtualAddress::identity(root);
        let ptr = va.as_ptr::<PageTable<L0>>();

        // SAFETY: `root` - свежевыделенный фрейм, доступный через identity-mapping pre-MMU и
        // эксклюзивно принадлежащий вызывающему. Запись пустой PageTable инициализирует все entries
        // в Invalid.
        unsafe {
            *ptr = PageTable::new();
        }

        ptr
    }
}

#[derive(Debug)]
pub(crate) enum MemorySetupError {
    OutOfMemory,
    #[allow(dead_code)]
    MappingFailed(memory::memory_mapper::MemoryMappingError),
    #[allow(dead_code)]
    FrameAllocatorInit(memory::frame_allocator::FrameAllocatorInitError),
}
