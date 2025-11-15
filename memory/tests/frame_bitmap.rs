mod common;

use common::{make_excluded, make_range, mock_backend};
use memory::FrameBitmap;
use memory::memory_range::MemoryRange;
use memory::physical::{Frame, PageAlignedAddress};
use std::collections::HashSet;

fn expected_bitmap_frames(region: &MemoryRange<PageAlignedAddress>) -> usize {
    let bytes = region.frame_count().div_ceil(8);
    bytes.div_ceil(region.frame_size)
}

fn range_frames(range: &MemoryRange<PageAlignedAddress>) -> Vec<Frame> {
    let start = Frame::from(range.start());
    let end = Frame::from(range.end());
    (start.number()..=end.number()).map(Frame::new).collect()
}

#[test]
fn reserves_bitmap_area_and_excluded_regions() {
    let backend = mock_backend(512);
    let region = make_range(0, 256);
    let excluded = make_excluded(&[(0, 4), (64, 8), (200, 16)]);

    let bitmap = FrameBitmap::new(&backend, &region, &excluded).expect("bitmap should be created");

    let mut allocated = Vec::new();
    let search_start = Frame::from(region.start());
    while let Some(frame) = bitmap.alloc_from(search_start) {
        allocated.push(frame);
    }

    let reserved_excluded: usize = excluded.iter().map(|r| r.frame_count()).sum();
    let reserved_bitmap = expected_bitmap_frames(&region);

    let unique_alloc: HashSet<usize> = allocated.iter().map(|frame| frame.number()).collect();
    assert_eq!(
        unique_alloc.len(),
        allocated.len(),
        "allocator should not return duplicate frames"
    );

    let bitmap_frame = Frame::from(bitmap.bitmap_address());
    assert!(
        !unique_alloc.contains(&bitmap_frame.number()),
        "frame used for bitmap storage must remain reserved"
    );

    let expected_free = region.frame_count() - reserved_excluded - reserved_bitmap;
    assert_eq!(
        unique_alloc.len(),
        expected_free,
        "allocator should provide every non-reserved frame exactly once"
    );

    assert_eq!(
        unique_alloc.len() + reserved_excluded + reserved_bitmap,
        region.frame_count(),
        "all frames must account either as free, excluded or bitmap storage"
    );

    let excluded_set: HashSet<usize> = excluded
        .iter()
        .flat_map(|range| range_frames(range))
        .map(|frame| frame.number())
        .collect();

    for frame in &allocated {
        assert!(
            !excluded_set.contains(&frame.number()),
            "allocator returned frame from excluded range"
        );
    }
}

#[test]
fn set_and_clear_range_controls_allocation() {
    let backend = mock_backend(128);
    let region = make_range(0, 64);
    let bitmap = FrameBitmap::new(&backend, &region, &[]).unwrap();

    let reserve_start = Frame::from(common::frame_to_address(8));
    let reserve_end = Frame::from(common::frame_to_address(16));
    bitmap.set_range_unchecked(reserve_start, reserve_end);

    let first = bitmap
        .alloc_from(Frame::from(region.start()))
        .expect("at least one frame should be free");
    assert!(
        first.number() < reserve_start.number() || first.number() >= reserve_end.number(),
        "reserved range should be skipped"
    );

    for frame in reserve_start.number()..reserve_end.number() {
        bitmap.clear(Frame::new(frame));
    }

    let recycled = bitmap
        .alloc_from(Frame::new(reserve_start.number()))
        .expect("freed frames should become available");
    assert!(
        (reserve_start.number()..reserve_end.number()).contains(&recycled.number()),
        "cleared frame must be recycled"
    );
}

#[test]
fn allocation_wraps_when_reaching_region_end() {
    let backend = mock_backend(64);
    let region = make_range(0, 32);
    let bitmap = FrameBitmap::new(&backend, &region, &[]).unwrap();

    let mut seen = Vec::new();
    let search_start = Frame::from(region.start());
    while let Some(frame) = bitmap.alloc_from(search_start) {
        seen.push(frame);
    }

    assert!(
        seen.len() > 0,
        "at least one frame should be allocated before exhaustion"
    );

    let first = seen[0];
    bitmap.clear(first);

    let wrap_offset = Frame::from(region.end());
    let wrapped = bitmap
        .alloc_from(wrap_offset)
        .expect("allocator should wrap to the beginning");
    assert_eq!(wrapped, first, "cleared frame must be reused via wrap");
}
