use memory::memory_range::MemoryRange;
use memory::physical::PageAlignedAddress;

pub const TEST_FRAME_SIZE: usize = PageAlignedAddress::alignment();

pub fn frame_to_address(frame: usize) -> PageAlignedAddress {
    PageAlignedAddress::from_usize(frame * TEST_FRAME_SIZE).expect("frame_to_address: not aligned")
}

pub fn make_range(start_frame: usize, frame_count: usize) -> MemoryRange<PageAlignedAddress> {
    assert!(frame_count > 0, "frame_count must be > 0");
    let start = frame_to_address(start_frame);
    let end = frame_to_address(start_frame + frame_count - 1);
    MemoryRange::new(start, end, TEST_FRAME_SIZE)
}

pub fn make_excluded(spec: &[(usize, usize)]) -> Vec<MemoryRange<PageAlignedAddress>> {
    spec.iter()
        .map(|(start, len)| make_range(*start, *len))
        .collect()
}
