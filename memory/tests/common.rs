use memory::aligned::Aligned;
use memory::memory_range::MemoryRange;
use memory::physical_address::PageAlignedAddress;
use memory::virtual_address::VirtualAddress;

pub const TEST_FRAME_SIZE: usize = PageAlignedAddress::ALIGNMENT;

pub fn frame_to_address(frame: usize) -> PageAlignedAddress {
    PageAlignedAddress::from_usize(frame * TEST_FRAME_SIZE).expect("frame_to_address: not aligned")
}

pub fn make_range(start_frame: usize, frame_count: usize) -> MemoryRange<PageAlignedAddress> {
    assert!(frame_count > 0, "frame_count must be > 0");
    let start = frame_to_address(start_frame);
    let end = frame_to_address(start_frame + frame_count - 1);
    MemoryRange::new(start, end)
}

pub fn va(addr: usize) -> VirtualAddress {
    VirtualAddress::new(addr)
}

pub fn make_va_range(start: usize, end: usize) -> MemoryRange<VirtualAddress> {
    MemoryRange::new(VirtualAddress::new(start), VirtualAddress::new(end))
}
