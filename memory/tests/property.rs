mod common;

use common::{TEST_FRAME_SIZE, make_excluded, make_range, mock_backend};
use memory::FrameBitmap;
use memory::physical::Frame;
use memory::physical_manager::{FrameAllocator, PhysicalMemoryManager};
use proptest::prelude::*;
use std::collections::HashSet;

const REGION_FRAMES: usize = 256;

proptest! {
    #[test]
    fn bitmap_never_allocates_reserved_frames(excluded in excluded_specs()) {
        let backend = mock_backend(REGION_FRAMES * 2);
        let region = make_range(0, REGION_FRAMES);
        let normalized = normalize_excluded(&excluded, REGION_FRAMES);
        let excluded_ranges = make_excluded(&normalized);

        let bitmap = FrameBitmap::new(&backend, &region, &excluded_ranges)
            .expect("bitmap must be created");

        let mut allocated = HashSet::new();
        while let Some(frame) = bitmap.alloc_from(Frame::from(region.start())) {
            allocated.insert(frame.number());
        }

        let expected_reserved: usize = normalized.iter().map(|(_, len)| *len).sum();
        let expected_bitmap = required_bitmap_frames(REGION_FRAMES);

        prop_assert_eq!(
            allocated.len(),
            REGION_FRAMES - expected_reserved - expected_bitmap,
            "all non-reserved frames should eventually be allocated"
        );
    }
}

proptest! {
    #[test]
    fn manager_allocation_respects_exclusions(excluded in excluded_specs()) {
        let backend = mock_backend(REGION_FRAMES * 2);
        let region = make_range(0, REGION_FRAMES);
        let normalized = normalize_excluded(&excluded, REGION_FRAMES);
        let excluded_ranges = make_excluded(&normalized);

        let manager =
            PhysicalMemoryManager::new(&backend, &region, &excluded_ranges).expect("manager");

        let mut allocated = HashSet::new();
        while let Some(frame) = manager.allocate_frame() {
            prop_assert!(
                !is_in_excluded(frame.number(), &normalized),
                "allocated frame must not belong to excluded ranges"
            );
            prop_assert!(allocated.insert(frame.number()), "no duplicate frames");
        }

       let expected_reserved: usize = normalized.iter().map(|(_, len)| *len).sum();
       let expected_bitmap = required_bitmap_frames(REGION_FRAMES);
       prop_assert_eq!(
           allocated.len(),
           REGION_FRAMES - expected_reserved - expected_bitmap,
           "allocator should produce every non-reserved frame exactly once"
        );
    }
}

#[cfg(feature = "mem-bench")]
#[test]
fn stress_allocate_free_cycles() {
    let backend = mock_backend(1024);
    let region = make_range(0, 512);
    let manager =
        PhysicalMemoryManager::new(&backend, &region, &[]).expect("manager should be created");

    for _ in 0..100 {
        let mut frames = Vec::new();
        while let Some(frame) = manager.allocate_frame() {
            frames.push(frame);
        }
        for frame in frames {
            manager.deallocate_frame(frame).expect("valid frame");
        }
    }

    assert!(
        manager.allocate_frame().is_some(),
        "allocator should still return frames after stress cycle"
    );
}

fn excluded_specs() -> impl Strategy<Value = Vec<(usize, usize)>> {
    prop::collection::vec((0usize..REGION_FRAMES, 1usize..=REGION_FRAMES / 4), 0..5)
}

fn normalize_excluded(specs: &[(usize, usize)], limit: usize) -> Vec<(usize, usize)> {
    let mut ranges = specs
        .iter()
        .map(|(start, len)| (*start % limit, *len))
        .collect::<Vec<_>>();
    ranges.sort_by_key(|(start, _)| *start);

    let mut normalized = Vec::new();
    let mut cursor = 0;
    for (start, len) in ranges {
        let start = start.max(cursor);
        if start >= limit {
            break;
        }
        let len = len.min(limit - start);
        if len == 0 {
            continue;
        }
        normalized.push((start, len));
        cursor = start + len;
    }
    normalized
}

fn is_in_excluded(frame_number: usize, ranges: &[(usize, usize)]) -> bool {
    ranges
        .iter()
        .any(|(start, len)| (frame_number >= *start) && (frame_number < *start + *len))
}

fn required_bitmap_frames(region_frames: usize) -> usize {
    let bytes = region_frames.div_ceil(8);
    bytes.div_ceil(TEST_FRAME_SIZE)
}
