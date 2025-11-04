use crate::memory::entry_flags::EntryFlags;
use memory::memory_range::MemoryRange;
use memory::physical::{AddressType, PageAlignedAddress};

/// Раскладка физической памяти ядра
pub struct MemoryLayout {
    pub kernel: MemoryRegion<PageAlignedAddress>,
    pub dtb: MemoryRegion<PageAlignedAddress>,
    pub heap: MemoryRegion<PageAlignedAddress>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegion<A: AddressType> {
    pub label: &'static str,
    pub start: A,
    pub end: A,
    pub flags: EntryFlags,
    pub frame_size: usize,
}

impl<A: AddressType> Into<MemoryRange<A>> for MemoryRegion<A> {
    fn into(self) -> MemoryRange<A> {
        MemoryRange::new(self.start, self.end, self.frame_size)
    }
}
