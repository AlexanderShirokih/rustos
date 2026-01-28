mod common;

use collections::MutexCell;
use common::make_range;
use memory::FrameBitmap;
use memory::frame::Frame;
use memory::frame_allocator::{
    FrameAllocator, FrameError, PhysicalFrameAllocator, ReserveFrameError,
};
use memory::memory_range::MemoryRange;
use memory::physical_address::PageAlignedAddress;
use std::collections::HashSet;

fn frame_allocator(
    region_frames: usize,
    excluded_specs: &[(usize, usize)],
) -> PhysicalFrameAllocator<MutexCell<FrameBitmap>> {
    let region = make_range(0, region_frames);
    PhysicalFrameAllocator::new(&region)
}

#[test]
fn reserve_exact_protects_frames() {
    let frame_allocator = frame_allocator(128, &[]);

    let reserve_start = Frame::from(common::frame_to_address(8));
    let reserve_end = Frame::from(common::frame_to_address(16));
    frame_allocator
        .reserve_frames_exact(reserve_start, reserve_end)
        .expect("reservation should succeed");

    let mut allocated = HashSet::new();
    while let Some(frame) = frame_allocator.allocate_frame() {
        assert!(
            frame.number() < reserve_start.number() || frame.number() >= reserve_end.number(),
            "reserved frame {} must not be allocated",
            frame.number()
        );
        allocated.insert(frame.number());
    }
    assert!(
        allocated.len() > 0,
        "should be able to allocate frames outside reserved region"
    );
}

#[test]
fn reserve_exact_out_of_bounds_is_error() {
    let frame_allocator = frame_allocator(32, &[]);
    let start = Frame::new(40);
    let end = Frame::new(42);

    let err = frame_allocator
        .reserve_frames_exact(start, end)
        .unwrap_err();
    match err {
        ReserveFrameError::OutOfTargetBoundary { from_inclusive, .. } => {
            assert_eq!(from_inclusive, start);
        }
    }
}

#[test]
fn allocate_and_deallocate_recycles_frames() {
    let frame_allocator = frame_allocator(32, &[]);

    let first = frame_allocator.allocate_frame().expect("frame available");
    let second = frame_allocator
        .allocate_frame()
        .expect("second frame available");

    // Выделяем остальные фреймы, чтобы аллокатор исчерпал пространство
    while frame_allocator.allocate_frame().is_some() {}

    frame_allocator
        .deallocate_frame(first)
        .expect("valid frame");
    frame_allocator
        .deallocate_frame(second)
        .expect("valid frame");

    let recycled_first = frame_allocator.allocate_frame().expect("should recycle");
    let recycled_second = frame_allocator
        .allocate_frame()
        .expect("should recycle second");

    let numbers = [recycled_first.number(), recycled_second.number()];
    assert!(
        numbers.contains(&first.number()) && numbers.contains(&second.number()),
        "freed frames should be reused"
    );
}

#[test]
fn deallocate_out_of_range_fails() {
    let frame_allocator = frame_allocator(16, &[]);
    let invalid_frame = Frame::new(64);

    let err = frame_allocator.deallocate_frame(invalid_frame).unwrap_err();
    assert!(matches!(err, FrameError::OutOfRange));
}

#[test]
fn allocation_stops_when_exhausted() {
    let frame_allocator = frame_allocator(16, &[(0, 4)]);

    let expected = available_frames(16, &[(0, 4)]);
    for _ in 0..expected {
        frame_allocator
            .allocate_frame()
            .expect("frame should be available");
    }
    assert!(
        frame_allocator.allocate_frame().is_none(),
        "allocator must return None when exhausted"
    );
}

fn available_frames(region_frames: usize, excluded_specs: &[(usize, usize)]) -> usize {
    let excluded: usize = excluded_specs.iter().map(|(_, len)| *len).sum();
    // Теперь bitmap не занимает фреймы, он выделяется через глобальный аллокатор
    region_frames.saturating_sub(excluded)
}

fn make_excluded(spec: &[(usize, usize)]) -> Vec<MemoryRange<PageAlignedAddress>> {
    spec.iter()
        .map(|(start, len)| make_range(*start, *len))
        .collect()
}
