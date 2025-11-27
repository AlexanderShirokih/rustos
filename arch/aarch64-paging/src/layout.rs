use crate::entry_flags::EntryFlags;
use memory::memory_range::MemoryRange;
use memory::physical::{AddressType, Aligned, PageAlignedAddress, PhysicalAddress};

const MAX_MEMORY_REGIONS: usize = 32;

/// Раскладка физической памяти ядра
pub struct MemoryLayout {
    regions: [MemoryRegion<PageAlignedAddress>; MAX_MEMORY_REGIONS],
    regions_count: usize,
}

impl MemoryLayout {
    pub fn new() -> Self {
        Self {
            regions: [MemoryRegion::empty(); MAX_MEMORY_REGIONS],
            regions_count: 0,
        }
    }

    pub fn add(&mut self, region: MemoryRegion<PageAlignedAddress>) {
        self.regions[self.regions_count] = region;
        self.regions_count += 1;
    }

    pub fn iter(&self) -> impl Iterator<Item = MemoryRegion<PageAlignedAddress>> {
        self.regions[0..self.regions_count].iter().copied()
    }

    pub fn heap(&self) -> impl Iterator<Item = MemoryRegion<PageAlignedAddress>> {
        self.iter()
            .filter(|&region| region.label.eq(MemoryRegion::HEAP))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegion<A: AddressType + Aligned> {
    pub label: &'static str,
    pub start: A,
    pub end: A,
    pub flags: EntryFlags,
    pub identity_map: bool,
}

impl MemoryRegion<PageAlignedAddress> {
    pub const HEAP: &'static str = "Heap";

    /// Создает пустой регион
    pub const fn empty() -> Self {
        Self {
            label: "",
            flags: EntryFlags::empty(),
            start: PageAlignedAddress::zero(),
            end: PageAlignedAddress::zero(),
            identity_map: false,
        }
    }

    pub fn frame_size(&self) -> usize {
        PageAlignedAddress::alignment()
    }

    pub fn new_raw(
        label: &'static str,
        start: &u8,
        end: &u8,
        flags: EntryFlags,
        identity_map: bool,
    ) -> Self {
        let start_addr = start as *const u8 as usize;
        let end_addr = end as *const u8 as usize;

        Self::new(label, start_addr, end_addr, flags, identity_map)
    }

    pub fn new(
        label: &'static str,
        start_addr: usize,
        end_addr: usize,
        flags: EntryFlags,
        identity_map: bool,
    ) -> Self {
        Self {
            label,
            flags,
            identity_map,
            start: PageAlignedAddress::aligned_down(PhysicalAddress::from(start_addr)),
            end: PageAlignedAddress::aligned_up(PhysicalAddress::from(end_addr)),
        }
    }
}

impl<A: AddressType + Aligned> Into<MemoryRange<A>> for MemoryRegion<A> {
    fn into(self) -> MemoryRange<A> {
        MemoryRange::new(self.start, self.end, A::alignment())
    }
}
