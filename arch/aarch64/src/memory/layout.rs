//! Раскладка физической памяти ядра.

use aarch64_paging::mem_flags::Aarch64MemFlags;
use aarch64_paging::preset::Mmio;
use collections::Vec;
use collections::interval_set::IntervalSet;
use klog::warn;
use memory::aligned::{Address, Aligned};
use memory::memory_range::MemoryRange;
use memory::physical_address::{PageAlignedAddress, PhysicalAddress};
use memory::virtual_address::PageAlignedVirtualAddress;

/// Максимальное количество регионов памяти.
pub const MAX_MEMORY_REGIONS: usize = 32;

/// Тег типа региона памяти
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionTag {
    /// Код и данные ядра
    Kernel,
    /// Куча (свободная RAM)
    Heap,
    /// Memory-mapped I/O
    Mmio,
    /// Неизвестный/служебный регион
    Other,
}

/// Раскладка физической памяти ядра.
pub struct MemoryLayout {
    /// Список регионов памяти.
    regions: Vec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
}

impl MemoryLayout {
    pub const fn new() -> Self {
        Self {
            regions: Vec::new(),
        }
    }

    pub fn add(&mut self, region: MemoryRegion<PageAlignedAddress>) {
        self.regions.push(region).expect("Too many memory regions");
    }

    pub fn iter(&self) -> impl Iterator<Item = &MemoryRegion<PageAlignedAddress>> {
        self.regions.iter()
    }

    /// Вычисляет свободные области heap (heap минус зарезервированные регионы)
    pub fn free_heap_regions(
        &self,
    ) -> Result<IntervalSet<PageAlignedAddress, MAX_MEMORY_REGIONS>, ()> {
        let mut free_regions = IntervalSet::<PageAlignedAddress, MAX_MEMORY_REGIONS>::new();

        // Добавление свободных областей (heap-регионы)
        for heap in self.iter().filter(|region| region.is_heap()) {
            if free_regions.add(heap.start, heap.end).is_none() {
                warn!("free_heap_regions"; "ERROR: Failed to add heap region");
                return Err(());
            }
        }

        // Вычитаем занятые (зарезервированные) области
        for region in self.iter().filter(|region| !region.is_heap()) {
            if free_regions.remove(region.start, region.end).is_none() {
                warn!("free_heap_regions"; "ERROR: Failed to remove reserved region");
                return Err(());
            }
        }

        Ok(free_regions)
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

impl MemoryRegion<PageAlignedAddress> {
    /// Создаёт регион из указателей на символы линкера.
    pub fn new_raw(tag: RegionTag, start: &u8, end: &u8, flags: Aarch64MemFlags) -> Self {
        let start_addr = start as *const u8 as usize;
        let end_addr = end as *const u8 as usize;

        Self::new(tag, start_addr, end_addr, flags)
    }

    /// Создаёт регион из адресов.
    pub const fn new(tag: RegionTag, start_addr: usize, end_addr: usize, flags: Aarch64MemFlags) -> Self {
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
        PageAlignedVirtualAddress::from_usize(higher_half_base + self.start.as_usize())
            .expect("address should be page aligned")
    }

    pub fn is_heap(&self) -> bool {
        self.tag == RegionTag::Heap
    }
}

impl<A: Address + Aligned> From<MemoryRegion<A>> for MemoryRange<A> {
    fn from(region: MemoryRegion<A>) -> Self {
        MemoryRange::new(region.start, region.end)
    }
}
