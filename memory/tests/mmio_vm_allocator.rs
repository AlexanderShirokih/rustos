use memory::mmio_vm_allocator::{MmioVmAllocator, MmioVmAllocError};
use memory::virtual_address::PageAlignedVirtualAddress;
use core::alloc::Layout;

const BASE: usize = 0x1000_0000_0000;
const SIZE_64K: usize = 0x1_0000;

fn pa_va(addr: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(addr).expect("page aligned")
}

#[test]
fn allocate_sequential() {
    let mut alloc = MmioVmAllocator::empty();
    alloc.add_range(pa_va(BASE), pa_va(BASE + SIZE_64K)).unwrap();

    let layout_3p = Layout::from_size_align(0x3000, 0x1000).unwrap();
    let layout_1p = Layout::from_size_align(0x1000, 0x1000).unwrap();

    let a1 = alloc.allocate(layout_3p).unwrap();
    let a2 = alloc.allocate(layout_1p).unwrap();

    assert_eq!(a1.as_usize(), BASE);
    assert_eq!(a2.as_usize(), BASE + 0x3000);
}

#[test]
fn allocation_respects_alignment() {
    let mut alloc = MmioVmAllocator::empty();
    alloc.add_range(pa_va(BASE + 0x1000), pa_va(BASE + 0x9000)).unwrap();

    let layout = Layout::from_size_align(0x1000, 0x4000).unwrap();
    let addr = alloc.allocate(layout).unwrap();

    assert_eq!(addr.as_usize(), BASE + 0x4000);
}

#[test]
fn free_and_reuse() {
    let mut alloc = MmioVmAllocator::empty();
    alloc.add_range(pa_va(BASE), pa_va(BASE + 0x8000)).unwrap();

    let layout = Layout::from_size_align(0x2000, 0x1000).unwrap();
    let a1 = alloc.allocate(layout).unwrap();
    let _a2 = alloc.allocate(layout).unwrap();

    alloc.free(a1, 0x2000).unwrap();
    let a3 = alloc.allocate(layout).unwrap();

    assert_eq!(a1.as_usize(), a3.as_usize());
}

#[test]
fn out_of_space() {
    let mut alloc = MmioVmAllocator::empty();
    alloc.add_range(pa_va(BASE), pa_va(BASE + 0x2000)).unwrap();

    let layout = Layout::from_size_align(0x3000, 0x1000).unwrap();
    let err = alloc.allocate(layout).unwrap_err();

    assert_eq!(err, MmioVmAllocError::OutOfSpace);
}

#[test]
fn size_zero_is_invalid() {
    let mut alloc = MmioVmAllocator::empty();
    alloc.add_range(pa_va(BASE), pa_va(BASE + SIZE_64K)).unwrap();

    let layout = Layout::from_size_align(0, 0x1000).unwrap();
    let err = alloc.allocate(layout).unwrap_err();

    assert_eq!(err, MmioVmAllocError::InvalidLayout);
}
