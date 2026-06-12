use memory::{memory_range::MemoryRange, physical_address::PhysicalAddress};

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
