use aarch64_paging::mem_flags::MemFlags;
use collections::Vec;
use memory::aligned::{Address, Aligned};
use memory::memory_range::MemoryRange;
use memory::physical_address::{PageAlignedAddress, PhysicalAddress};
use memory::virtual_address::PageAlignedVirtualAddress;

const MAX_MEMORY_REGIONS: usize = 32;

/// Раскладка физической памяти ядра
pub struct MemoryLayout {
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegion<A: Address + Aligned> {
    pub label: &'static str,
    pub start: A,
    pub end: A,
    pub flags: MemFlags,
    pub va_offset: usize,
}

impl MemoryRegion<PageAlignedAddress> {
    pub const HEAP: &'static str = "Heap";

    pub fn new_raw(
        label: &'static str,
        start: &u8,
        end: &u8,
        flags: MemFlags,
        va_offset: usize,
    ) -> Self {
        let start_addr = start as *const u8 as usize;
        let end_addr = end as *const u8 as usize;

        Self::new(label, start_addr, end_addr, flags, va_offset)
    }

    pub const fn new(
        label: &'static str,
        start_addr: usize,
        end_addr: usize,
        flags: MemFlags,
        va_offset: usize,
    ) -> Self {
        assert!(va_offset % 0x1000 == 0, "va_offset must be aligned to 4KiB");

        Self {
            label,
            flags,
            va_offset,
            start: PageAlignedAddress::aligned_down(PhysicalAddress::new(start_addr)),
            end: PageAlignedAddress::aligned_up(PhysicalAddress::new(end_addr)),
        }
    }

    pub fn identity(
        label: &'static str,
        start_addr: usize,
        end_addr: usize,
        flags: MemFlags,
    ) -> Self {
        Self::new(label, start_addr, end_addr, flags, 0)
    }

    pub fn identity_raw(label: &'static str, start: &u8, end: &u8, flags: MemFlags) -> Self {
        Self::new_raw(label, start, end, flags, 0)
    }

    pub fn size(&self) -> usize {
        self.end.as_usize().saturating_sub(self.start.as_usize())
    }

    pub fn virtual_start(&self) -> PageAlignedVirtualAddress {
        PageAlignedVirtualAddress::identity(self.start)
            .offset(self.va_offset)
            .unwrap()
    }

    pub fn virtual_end(&self) -> PageAlignedVirtualAddress {
        PageAlignedVirtualAddress::identity(self.start)
            .offset(self.va_offset)
            .unwrap()
    }

    pub fn is_heap(&self) -> bool {
        self.label == Self::HEAP
    }

    pub fn is_identity(&self) -> bool {
        self.va_offset == 0
    }
}

impl<A: Address + Aligned> Into<MemoryRange<A>> for MemoryRegion<A> {
    fn into(self) -> MemoryRange<A> {
        MemoryRange::new(self.start, self.end)
    }
}
