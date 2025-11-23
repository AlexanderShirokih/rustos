use aarch64_paging::entry_flags::EntryFlags;
use aarch64_paging::layout::{MemoryLayout, MemoryRegion};
use core::assert;
use memory::memory_backend::MockMemoryBackend;
use memory::memory_range::MemoryRange;
use memory::physical::PageAlignedAddress;
use memory::physical_manager::PhysicalMemoryManager;

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
        frame_size: TEST_FRAME_SIZE,
    }
}

pub fn make_empty_region(
    label: &'static str,
    flags: EntryFlags,
) -> MemoryRegion<PageAlignedAddress> {
    let addr = frame_to_address(0);
    MemoryRegion {
        label,
        start: addr,
        end: addr,
        flags,
        frame_size: TEST_FRAME_SIZE,
    }
}

pub fn mock_backend(total_frames: usize) -> MockMemoryBackend {
    MockMemoryBackend::new(TEST_FRAME_SIZE, total_frames)
}

pub fn excluded_regions(specs: &[(usize, usize)]) -> Vec<MemoryRange<PageAlignedAddress>> {
    specs
        .iter()
        .map(|(start, len)| make_range(*start, *len))
        .collect()
}

pub fn build_frame_allocator<'a>(
    backend: &'a MockMemoryBackend,
    total_frames: usize,
    excluded: &[(usize, usize)],
) -> PhysicalMemoryManager<'a, MockMemoryBackend> {
    let region = make_range(0, total_frames);
    let excluded_regions = excluded_regions(excluded);
    PhysicalMemoryManager::new(backend, &region, &excluded_regions)
        .expect("frame allocator should be created")
}

pub fn basic_layout(heap_start_frame: usize, heap_frames: usize) -> MemoryLayout {
    MemoryLayout {
        kernel_code: make_region("kernel_code", 0, 1, EntryFlags::KERNEL_CODE),
        kernel_rodata: make_empty_region("kernel_rodata", EntryFlags::KERNEL_RODATA),
        kernel_data: make_empty_region("kernel_data", EntryFlags::KERNEL_DATA),
        dtb: make_region("dtb", 1, 1, EntryFlags::DEVICE),
        heap: make_region(
            "heap",
            heap_start_frame,
            heap_frames,
            EntryFlags::KERNEL_DATA,
        ),
        additional: make_empty_region("additional", EntryFlags::KERNEL_DATA),
    }
}

pub fn layout_from_regions(
    kernel: MemoryRegion<PageAlignedAddress>,
    dtb: MemoryRegion<PageAlignedAddress>,
    heap: MemoryRegion<PageAlignedAddress>,
) -> MemoryLayout {
    MemoryLayout {
        kernel_code: kernel,
        kernel_rodata: make_empty_region("kernel_rodata", EntryFlags::KERNEL_RODATA),
        kernel_data: make_empty_region("kernel_data", EntryFlags::KERNEL_DATA),
        dtb,
        heap,
        additional: make_empty_region("additional", EntryFlags::KERNEL_DATA),
    }
}
