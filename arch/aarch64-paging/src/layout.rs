use crate::entry_flags::EntryFlags;
use memory::memory_range::MemoryRange;
use memory::physical::{AddressType, PageAlignedAddress, PhysicalAddress};

/// Раскладка физической памяти ядра
pub struct MemoryLayout {
    pub kernel_code: MemoryRegion<PageAlignedAddress>,
    pub kernel_rodata: MemoryRegion<PageAlignedAddress>,
    pub kernel_data: MemoryRegion<PageAlignedAddress>,
    pub dtb: MemoryRegion<PageAlignedAddress>,
    pub heap: MemoryRegion<PageAlignedAddress>,
    pub additional: MemoryRegion<PageAlignedAddress>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegion<A: AddressType> {
    pub label: &'static str,
    pub start: A,
    pub end: A,
    pub flags: EntryFlags,
    pub frame_size: usize,
}

impl MemoryRegion<PageAlignedAddress> {
    pub fn new_raw(label: &'static str, start: &u8, end: &u8, flags: EntryFlags) -> Self {
        let start_addr = start as *const u8 as usize;
        let end_addr = end as *const u8 as usize;

        Self::new(label, start_addr, end_addr, flags)
    }

    pub fn new(label: &'static str, start_addr: usize, end_addr: usize, flags: EntryFlags) -> Self {
        Self {
            label,
            flags,
            start: PageAlignedAddress::aligned_down(PhysicalAddress::from(start_addr)),
            end: PageAlignedAddress::aligned_up(PhysicalAddress::from(end_addr)),
            frame_size: PageAlignedAddress::alignment(),
        }
    }
}

impl<A: AddressType> Into<MemoryRange<A>> for MemoryRegion<A> {
    fn into(self) -> MemoryRange<A> {
        MemoryRange::new(self.start, self.end, self.frame_size)
    }
}
