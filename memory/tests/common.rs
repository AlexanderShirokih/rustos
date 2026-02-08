use memory::aligned::Aligned;
use memory::memory_range::MemoryRange;
use memory::physical_address::PageAlignedAddress;

// Каждый тест-файл в tests/ компилируется как отдельный crate.
// Функции, используемые в одних тестах, показываются как dead_code при компиляции других.
#[allow(dead_code)]
const TEST_FRAME_SIZE: usize = PageAlignedAddress::ALIGNMENT;

#[allow(dead_code)]
pub fn frame_to_address(frame: usize) -> PageAlignedAddress {
    PageAlignedAddress::from_usize(frame * TEST_FRAME_SIZE).expect("frame_to_address: not aligned")
}

#[allow(dead_code)]
pub fn make_range(start_frame: usize, frame_count: usize) -> MemoryRange<PageAlignedAddress> {
    assert!(frame_count > 0, "frame_count must be > 0");
    let start = frame_to_address(start_frame);
    let end = frame_to_address(start_frame + frame_count - 1);
    MemoryRange::new(start, end)
}
