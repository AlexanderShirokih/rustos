#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use collections::{LockCell, MutexCell};
use common::nz;
use memory::{
    AccessMask, MemFlags, MemoryRegion,
    memory_mapper::MemoryMapper,
    physical_address::PageAlignedAddress,
    testing::{PageMapper, UnmapCall},
    user_vm_allocator::{MappingTag, UserVmAllocator},
    user_vm_context::UserVmContext,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

const PAGE: usize = memory::PAGE_SIZE.get();
const ARENA: usize = 0x4000_0000;
const REGION_PA: usize = 0x8000_0000;

fn region() -> Arc<MemoryRegion> {
    Arc::new(MemoryRegion::create_physical_device(
        PageAlignedAddress::from_usize(REGION_PA).unwrap(),
        nz(PAGE),
        AccessMask::RW,
    ))
}

fn setup() -> (UserVmContext, Arc<PageMapper>, PageAlignedVirtualAddress) {
    let mapper = Arc::new(PageMapper::new());
    let mut alloc = UserVmAllocator::new(
        PageAlignedVirtualAddress::from_usize(ARENA).unwrap(),
        VirtualAddress::new(ARENA + 16 * PAGE),
    );
    let allocated = alloc
        .allocate(
            nz(PAGE),
            MappingTag {
                flags: MemFlags::user_rw(),
                region: region(),
                grant: AccessMask::RW,
                revocation: None,
            },
        )
        .unwrap();
    let base = allocated.base();
    mapper
        .map_exact(
            base,
            PageAlignedAddress::from_usize(REGION_PA).unwrap(),
            PAGE,
            MemFlags::user_rw(),
        )
        .unwrap();
    let ctx = UserVmContext::new(mapper.clone(), Arc::new(MutexCell::new(alloc)));
    (ctx, mapper, base)
}

#[test]
fn unmap_range_unmaps_and_frees() {
    let (ctx, mapper, base) = setup();
    ctx.unmap_range(base, nz(PAGE));

    assert_eq!(
        mapper.unmaps(),
        [UnmapCall {
            va: base.as_usize(),
            size: PAGE
        }]
    );
    ctx.allocator()
        .with_lock(|a| assert!(a.lookup(base, nz(PAGE)).is_err()));
}

#[test]
fn unmap_range_is_idempotent() {
    let (ctx, mapper, base) = setup();
    ctx.unmap_range(base, nz(PAGE));
    ctx.unmap_range(base, nz(PAGE));

    assert_eq!(mapper.unmaps().len(), 1);
}

#[test]
fn drain_releases_mappings_while_an_allocator_clone_survives() {
    let (ctx, mapper, base) = setup();
    let allocator = ctx.allocator().clone();

    ctx.drain();
    drop(ctx);

    assert_eq!(
        mapper.unmaps(),
        [UnmapCall {
            va: base.as_usize(),
            size: PAGE
        }]
    );
    allocator.with_lock(|a| assert_eq!(a.live_count(), 0));
}
