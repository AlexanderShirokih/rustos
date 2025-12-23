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

#[test]
fn test_mock_memory_access_provider_basic() {
    use memory::memory::MemoryAccessProvider;
    use memory::test_utils::MockMemoryAccessProvider;

    let memory = MockMemoryAccessProvider::new(4096, 10);

    assert_eq!(memory.total_frames(), 10);

    let addr = VirtualAddress::new(0);
    memory.write(addr, &42u32);
    let value: u32 = memory.read(addr);
    assert_eq!(value, 42);
}
