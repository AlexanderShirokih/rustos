mod common;

use common::make_range;
use memory::FrameBitmap;
use memory::physical::Frame;

#[test]
fn set_and_clear_range_controls_allocation() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(&region);

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
    let region = make_range(0, 32);
    let mut bitmap = FrameBitmap::new(&region);

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
