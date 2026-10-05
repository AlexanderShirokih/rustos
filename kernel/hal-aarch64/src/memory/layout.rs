//! Раскладка физической памяти ядра.

use hal_aarch64_paging::{mem_flags::Aarch64MemFlags, preset::Mmio};
use io::Vec;
use kbase::interval_set::StaticIntervalSet;
use memory::{
    aligned::{Address, Aligned},
    memory_range::MemoryRange,
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

pub(crate) const MAX_MEMORY_REGIONS: usize = 128;

/// Тег типа региона памяти
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionTag {
    /// Код и данные ядра
    Kernel,
    /// Куча (свободная RAM)
    Heap,
    /// Memory-mapped I/O
    Mmio,
    /// Зарезервированная память, которую ядро не отображает и аллокатор не выдаёт
    Unmapped,
    /// Неизвестный/служебный регион
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TooManyMemoryRegions;

pub(crate) struct MemoryLayout {
    regions: Vec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
}

impl MemoryLayout {
    pub(crate) const fn new() -> Self {
        Self {
            regions: Vec::new(),
        }
    }

    pub(crate) fn add(
        &mut self,
        region: MemoryRegion<PageAlignedAddress>,
    ) -> Result<(), TooManyMemoryRegions> {
        self.regions.push(region).ok_or(TooManyMemoryRegions)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &MemoryRegion<PageAlignedAddress>> {
        self.regions.iter()
    }

    pub(crate) fn free_heap_regions(
        &self,
    ) -> Result<StaticIntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>, ()> {
        let mut free_regions = StaticIntervalSet::<PageAlignedAddress, MAX_MEMORY_REGIONS>::new();

        // Добавление свободных областей (heap-регионы)
        for heap in self.iter().filter(|region| region.is_heap()) {
            if free_regions.add(heap.start, heap.end).is_none() {
                return Err(());
            }
        }

        // Вычитаем занятые (зарезервированные) области
        for region in self.iter().filter(|region| !region.is_heap()) {
            if free_regions.remove(region.start, region.end).is_none() {
                return Err(());
            }
        }

        Ok(free_regions)
    }

    pub(crate) fn push_mapped_non_heap_regions(
        &self,
        out: &mut Vec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
    ) -> Result<(), TooManyMemoryRegions> {
        for region in self
            .iter()
            .filter(|region| !region.is_heap() && region.tag != RegionTag::Unmapped)
        {
            if region.tag == RegionTag::Kernel {
                out.push(*region).ok_or(TooManyMemoryRegions)?;
                continue;
            }

            let mut pieces = StaticIntervalSet::<PageAlignedAddress, MAX_MEMORY_REGIONS>::new();
            pieces
                .add(region.start, region.end)
                .ok_or(TooManyMemoryRegions)?;

            for hole in self.iter().filter(|hole| hole.tag == RegionTag::Unmapped) {
                pieces
                    .remove(hole.start, hole.end)
                    .ok_or(TooManyMemoryRegions)?;
            }

            for piece in pieces.iter() {
                out.push(MemoryRegion {
                    start: piece.start,
                    end: piece.end,
                    ..*region
                })
                .ok_or(TooManyMemoryRegions)?;
            }
        }

        Ok(())
    }
}

/// Регион физической памяти.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegion<A: Address + Aligned> {
    /// Тип региона.
    pub tag: RegionTag,
    /// Начальный адрес (включительно).
    pub start: A,
    /// Конечный адрес (исключительно).
    pub end: A,
    /// Атрибуты памяти.
    pub flags: Aarch64MemFlags,
}

#[derive(Clone, Copy)]
pub(crate) struct LinkTimeRange {
    start: usize,
    end: usize,
}

impl LinkTimeRange {
    // SAFETY: вызывающий обязан находиться в pre-MMU фазе. Только там PC, а с ним и значение
    // символа, совпадает с физическим адресом секции.
    pub(crate) unsafe fn physical(start: &u8, end: &u8) -> Self {
        Self {
            start: core::ptr::from_ref::<u8>(start) as usize,
            end: core::ptr::from_ref::<u8>(end) as usize,
        }
    }

    pub(crate) fn into_region(
        self,
        tag: RegionTag,
        flags: Aarch64MemFlags,
    ) -> MemoryRegion<PageAlignedAddress> {
        MemoryRegion::new(tag, self.start, self.end, flags)
    }
}

impl MemoryRegion<PageAlignedAddress> {
    /// Создаёт регион из физических адресов.
    pub const fn new(
        tag: RegionTag,
        start_addr: usize,
        end_addr: usize,
        flags: Aarch64MemFlags,
    ) -> Self {
        Self {
            tag,
            flags,
            start: PageAlignedAddress::aligned_down(PhysicalAddress::new(start_addr)),
            end: PageAlignedAddress::aligned_up(PhysicalAddress::new(end_addr)),
        }
    }

    pub const fn mmio(start_addr: usize, size: usize) -> Self {
        Self::new(
            RegionTag::Mmio,
            start_addr,
            start_addr + size,
            Mmio::flags(),
        )
    }

    /// Возвращает размер региона в байтах.
    pub fn size(&self) -> usize {
        self.end.as_usize().saturating_sub(self.start.as_usize())
    }

    /// Возвращает виртуальный адрес в higher half.
    pub fn virtual_start(&self, higher_half_base: usize) -> PageAlignedVirtualAddress {
        // База higher-half и start выровнены по странице, сумма тоже.
        debug_assert!(higher_half_base.is_multiple_of(PageAlignedVirtualAddress::ALIGNMENT));
        PageAlignedVirtualAddress::aligned_down(VirtualAddress::new(
            higher_half_base + self.start.as_usize(),
        ))
    }

    pub fn is_heap(&self) -> bool {
        self.tag == RegionTag::Heap
    }
}

impl From<MemoryRegion<PageAlignedAddress>> for MemoryRange<PageAlignedAddress> {
    fn from(region: MemoryRegion<PageAlignedAddress>) -> Self {
        // `MemoryRegion` и `MemoryRange` оба полуоткрытые [start, end).
        MemoryRange::new(region.start, region.end)
    }
}
