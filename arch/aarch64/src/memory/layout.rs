use aarch64_paging::mem_flags::MemFlags;
use collections::Vec;
use memory::aligned::{Address, Aligned};
use memory::memory_range::MemoryRange;
use memory::physical_address::{PageAlignedAddress, PhysicalAddress};

const MAX_MEMORY_REGIONS: usize = 32;

/// Раскладка физической памяти ядра
pub struct MemoryLayout {
    regions: Vec<MemoryRegion<PageAlignedAddress>, MAX_MEMORY_REGIONS>,
}

impl MemoryLayout {
    pub fn new() -> Self {
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

    pub fn heap(&self) -> impl Iterator<Item = &MemoryRegion<PageAlignedAddress>> {
        self.iter()
            .filter(|&region| region.label.eq(MemoryRegion::HEAP))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegion<A: Address + Aligned + Copy> {
    pub label: &'static str,
    pub start: A,
    pub end: A,
    pub flags: MemFlags,
    pub identity_map: bool,
}

impl MemoryRegion<PageAlignedAddress> {
    pub const HEAP: &'static str = "Heap";

    pub fn new_raw(
        label: &'static str,
        start: &u8,
        end: &u8,
        flags: MemFlags,
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
        flags: MemFlags,
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

impl<A: Aligned + Address> Into<MemoryRange<A>> for MemoryRegion<A> {
    fn into(self) -> MemoryRange<A> {
        MemoryRange::new(self.start, self.end)
    }
}
