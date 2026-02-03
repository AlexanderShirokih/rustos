use memory::memory_range::MemoryRange;
use memory::physical_address::PhysicalAddress;

#[test]
fn size_is_inclusive() {
    let range = MemoryRange::new(PhysicalAddress::new(100), PhysicalAddress::new(199));
    assert_eq!(range.size(), 100);
}

#[test]
fn size_of_single_address_is_one() {
    let range = MemoryRange::new(PhysicalAddress::new(42), PhysicalAddress::new(42));
    assert_eq!(range.size(), 1);
}
