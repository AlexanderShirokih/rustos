mod common;

use common::{make_excluded, make_range};
use memory::physical::Frame;
use memory::physical_manager::{
    FrameAllocator, FrameError, PhysicalMemoryManager, ReserveFrameError,
};
use std::collections::HashSet;

fn build_manager(region_frames: usize, excluded_specs: &[(usize, usize)]) -> PhysicalMemoryManager {
    let region = make_range(0, region_frames);
    let excluded = make_excluded(excluded_specs);
    PhysicalMemoryManager::new(&region, excluded.into_iter())
}

#[test]
fn reserve_exact_protects_frames() {
    let manager = build_manager(128, &[]);

    let reserve_start = Frame::from(common::frame_to_address(8));
    let reserve_end = Frame::from(common::frame_to_address(16));
    manager
        .reserve_frames_exact(reserve_start, reserve_end)
        .expect("reservation should succeed");

    let mut allocated = HashSet::new();
    while let Some(frame) = manager.allocate_frame() {
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
    let manager = build_manager(32, &[]);
    let start = Frame::new(40);
    let end = Frame::new(42);

    let err = manager.reserve_frames_exact(start, end).unwrap_err();
    match err {
        ReserveFrameError::OutOfTargetBoundary { from_inclusive, .. } => {
            assert_eq!(from_inclusive, start);
        }
    }
}

#[test]
fn allocate_and_deallocate_recycles_frames() {
    let manager = build_manager(32, &[]);

    let first = manager.allocate_frame().expect("frame available");
    let second = manager.allocate_frame().expect("second frame available");

    // Выделяем остальные фреймы, чтобы аллокатор исчерпал пространство
    while manager.allocate_frame().is_some() {}

    manager.deallocate_frame(first).expect("valid frame");
    manager.deallocate_frame(second).expect("valid frame");

    let recycled_first = manager.allocate_frame().expect("should recycle");
    let recycled_second = manager.allocate_frame().expect("should recycle second");

    let numbers = [recycled_first.number(), recycled_second.number()];
    assert!(
        numbers.contains(&first.number()) && numbers.contains(&second.number()),
        "freed frames should be reused"
    );
}

#[test]
fn deallocate_out_of_range_fails() {
    let manager = build_manager(16, &[]);
    let invalid_frame = Frame::new(64);

    let err = manager.deallocate_frame(invalid_frame).unwrap_err();
    assert!(matches!(err, FrameError::OutOfRange));
}

#[test]
fn allocation_stops_when_exhausted() {
    let manager = build_manager(16, &[(0, 4)]);

    let expected = available_frames(16, &[(0, 4)]);
    for _ in 0..expected {
        manager.allocate_frame().expect("frame should be available");
    }
    assert!(
        manager.allocate_frame().is_none(),
        "allocator must return None when exhausted"
    );
}

fn available_frames(region_frames: usize, excluded_specs: &[(usize, usize)]) -> usize {
    let excluded: usize = excluded_specs.iter().map(|(_, len)| *len).sum();
    // Теперь bitmap не занимает фреймы, он выделяется через глобальный аллокатор
    region_frames.saturating_sub(excluded)
}
