use memory::{
    aligned::Aligned,
    memory_range::MemoryRange,
    physical_address::{PageAlignedAddress, PhysicalAddress},
};

const PAGE: usize = PageAlignedAddress::ALIGNMENT;

fn page(addr: usize) -> PageAlignedAddress {
    PageAlignedAddress::from_usize(addr).expect("address must be page-aligned")
}

#[test]
fn size_is_half_open_span() {
    let range = MemoryRange::new(PhysicalAddress::new(100), PhysicalAddress::new(200));
    assert_eq!(range.size(), 100);
}

#[test]
fn empty_range_has_zero_size() {
    let range = MemoryRange::new(PhysicalAddress::new(42), PhysicalAddress::new(42));
    assert_eq!(range.size(), 0);
}

#[test]
fn size_is_zero_when_end_below_start() {
    // size() использует saturating_sub: вырожденный диапазон не уходит в underflow.
    let range = MemoryRange::new(PhysicalAddress::new(200), PhysicalAddress::new(100));
    assert_eq!(range.size(), 0);
}

#[test]
fn contains_respects_half_open_boundaries() {
    let range = MemoryRange::new(PhysicalAddress::new(100), PhysicalAddress::new(200));

    // start включён.
    assert!(range.contains(PhysicalAddress::new(100)));
    // внутренняя точка.
    assert!(range.contains(PhysicalAddress::new(150)));
    // последний валидный адрес.
    assert!(range.contains(PhysicalAddress::new(199)));
    // end исключён.
    assert!(!range.contains(PhysicalAddress::new(200)));
    // до начала.
    assert!(!range.contains(PhysicalAddress::new(99)));
}

#[test]
fn contains_is_always_false_for_empty_range() {
    let range = MemoryRange::new(PhysicalAddress::new(50), PhysicalAddress::new(50));
    assert!(!range.contains(PhysicalAddress::new(50)));
    assert!(!range.contains(PhysicalAddress::new(49)));
}

#[test]
fn frame_count_counts_pages_in_span() {
    // [PAGE*2, PAGE*5) == 3 фрейма.
    let range = MemoryRange::new(page(PAGE * 2), page(PAGE * 5));
    assert_eq!(range.frame_count(), 3);
}

#[test]
fn frame_count_is_zero_for_empty_range() {
    let range = MemoryRange::new(page(PAGE * 3), page(PAGE * 3));
    assert_eq!(range.frame_count(), 0);
}

#[test]
fn iter_yields_each_page_aligned_address() {
    let range = MemoryRange::new(page(PAGE * 2), page(PAGE * 5));

    let addrs: Vec<usize> = range.iter().map(PageAlignedAddress::as_usize).collect();
    assert_eq!(addrs, vec![PAGE * 2, PAGE * 3, PAGE * 4]);
}

#[test]
fn iter_on_empty_range_yields_nothing() {
    let range = MemoryRange::new(page(PAGE), page(PAGE));
    assert_eq!(range.iter().count(), 0);
}

#[test]
fn into_iter_matches_frame_count() {
    let range = MemoryRange::new(page(0), page(PAGE * 4));
    assert_eq!((&range).into_iter().count(), range.frame_count());
}
