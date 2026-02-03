use core::alloc::Layout;
use memory::aligned::Aligned;
use memory::bump_allocator::{BumpAllocError, BumpAllocator};
use memory::physical_address::PageAlignedAddress;

#[test]
fn allocate_detects_address_overflow() {
    let align = PageAlignedAddress::ALIGNMENT;
    let base = usize::MAX - (align - 1);
    let start = PageAlignedAddress::from_usize(base).expect("aligned start");
    let end = start;

    let mut allocator = BumpAllocator::new(start, end);
    let layout = Layout::from_size_align(align + 1, 8).expect("layout");

    let err = allocator.allocate(layout).unwrap_err();
    assert!(matches!(err, BumpAllocError::AddressOverflow));
}
