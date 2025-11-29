use aarch64_paging::entry_flags::EntryFlags;
use aarch64_paging::layout::{MemoryLayout, MemoryRegion};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::assert;
use memory::memory_range::MemoryRange;
use memory::physical::PageAlignedAddress;
use memory::physical_manager::PhysicalMemoryManager;

extern crate alloc;

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

pub fn make_region(
    label: &'static str,
    start_frame: usize,
    frame_count: usize,
    flags: EntryFlags,
) -> MemoryRegion<PageAlignedAddress> {
    let range = make_range(start_frame, frame_count);
    MemoryRegion {
        label,
        start: range.start(),
        end: range.end(),
        flags,
        identity_map: false,
    }
}

pub fn excluded_regions(specs: &[(usize, usize)]) -> Vec<MemoryRange<PageAlignedAddress>> {
    specs
        .iter()
        .map(|(start, len)| make_range(*start, *len))
        .collect()
}

pub fn build_frame_allocator(
    total_frames: usize,
    excluded: &[(usize, usize)],
) -> Arc<PhysicalMemoryManager> {
    let region = make_range(0, total_frames);
    let excluded_regions = excluded_regions(excluded);
    Arc::new(PhysicalMemoryManager::new(
        &region,
        excluded_regions.into_iter(),
    ))
}

pub fn layout_from_regions(
    kernel: MemoryRegion<PageAlignedAddress>,
    dtb: MemoryRegion<PageAlignedAddress>,
    heap: MemoryRegion<PageAlignedAddress>,
) -> MemoryLayout {
    let mut layout = MemoryLayout::new();
    layout.add(kernel);
    layout.add(dtb);
    layout.add(heap);
    layout
}
