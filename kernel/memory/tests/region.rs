#![allow(clippy::unwrap_used, clippy::unwrap_in_result)]

mod common;

use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
};
use std::sync::{Arc, Mutex};

use common::{make_range, nz};
use kbase::MutexCell;
use memory::{
    AccessMask, BudgetRefund, FrameBitmap, MemFlags, MemoryProfile, MemoryRegion,
    RegionCreateError, RegionSliceError, SliceProfile,
    frame::Frame,
    frame_allocator::{FrameAllocator, FrameError, PhysicalFrameAllocator, ReserveFrameError},
    memory_mapper::MemoryMappingError,
    physical_address::PageAlignedAddress,
    testing::{Call, MapExactCall, PageMapper, UnmapCall},
    virtual_address::PageAlignedVirtualAddress,
};

const PAGE_SIZE: usize = memory::PAGE_SIZE.get();

struct MockFrameAllocator {
    next: Mutex<usize>,
    allocated: Mutex<Vec<Frame>>,
    deallocated: Mutex<Vec<Frame>>,
    allocate_limit: Mutex<usize>,
}

impl MockFrameAllocator {
    fn new() -> Self {
        Self {
            next: Mutex::new(100),
            allocated: Mutex::new(Vec::new()),
            deallocated: Mutex::new(Vec::new()),
            allocate_limit: Mutex::new(usize::MAX),
        }
    }

    fn with_limit(limit: usize) -> Self {
        let me = Self::new();
        *me.allocate_limit.lock().unwrap() = limit;
        me
    }

    fn allocated(&self) -> Vec<Frame> {
        self.allocated.lock().unwrap().clone()
    }

    fn deallocated(&self) -> Vec<Frame> {
        self.deallocated.lock().unwrap().clone()
    }
}

impl FrameAllocator for MockFrameAllocator {
    fn reserve_frames_exact(
        &self,
        from_inclusive: Frame,
        _to_exclusive: Frame,
    ) -> Result<Frame, ReserveFrameError> {
        Ok(from_inclusive)
    }

    fn allocate_frame(&self) -> Option<Frame> {
        let mut limit = self.allocate_limit.lock().unwrap();
        if *limit == 0 {
            return None;
        }
        *limit -= 1;
        drop(limit);
        let mut n = self.next.lock().unwrap();
        let frame = Frame::new(*n);
        *n += 1;
        self.allocated.lock().unwrap().push(frame);
        Some(frame)
    }

    fn allocate_frames(&self, _count: NonZeroUsize) -> Option<Frame> {
        None
    }

    fn deallocate_frame(&self, frame: Frame) -> Result<(), FrameError> {
        self.deallocated.lock().unwrap().push(frame);
        Ok(())
    }

    fn is_allocated(&self, _frame: Frame) -> bool {
        false
    }
}

fn aligned_va(addr: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(addr).unwrap()
}

fn aligned_pa(addr: usize) -> PageAlignedAddress {
    PageAlignedAddress::from_usize(addr).unwrap()
}

fn leak_fa() -> &'static MockFrameAllocator {
    Box::leak(Box::new(MockFrameAllocator::new()))
}

#[test]
fn access_mask_allows_all_combinations() {
    assert!(AccessMask::RW.allows(AccessMask::R));
    assert!(AccessMask::RW.allows(AccessMask::W));
    assert!(AccessMask::RW.allows(AccessMask::RW));
    assert!(!AccessMask::RW.allows(AccessMask::X));
    assert!(!AccessMask::RW.allows(AccessMask::RX));
    assert!(AccessMask::RWX.allows(AccessMask::RWX));
    assert!(AccessMask::RX.allows(AccessMask::R));
    assert!(AccessMask::RX.allows(AccessMask::X));
    assert!(!AccessMask::RX.allows(AccessMask::W));
    assert!(AccessMask::NONE.allows(AccessMask::NONE));
    assert!(!AccessMask::NONE.allows(AccessMask::R));
}

#[test]
fn access_mask_from_bits_truncates_unknown() {
    let masked = AccessMask::from_bits_truncate(0xFF);
    assert_eq!(masked.bits(), AccessMask::RWX.bits());
}

#[test]
fn virtual_drop_returns_frames_to_allocator() {
    let fa = leak_fa();
    let region =
        MemoryRegion::create_virtual(fa, nz(3), AccessMask::RW).expect("alloc must succeed");
    assert_eq!(region.size_bytes().get(), 3 * PAGE_SIZE);
    assert_eq!(region.kind_tag(), 1);
    assert_eq!(fa.deallocated().len(), 0);
    drop(region);
    assert_eq!(fa.deallocated(), fa.allocated());
}

#[test]
fn virtual_create_oom_returns_all_frames() {
    let fa = Box::leak(Box::new(MockFrameAllocator::with_limit(2)));
    let err = MemoryRegion::create_virtual(fa, nz(5), AccessMask::RW);
    assert!(err.is_err());
    assert_eq!(err.err().unwrap(), RegionCreateError::OutOfMemory);
    assert_eq!(fa.deallocated().len(), 2);
}

#[test]
fn slice_physical_narrows_pa_size_and_access() {
    let region =
        MemoryRegion::create_physical_device(aligned_pa(0x4000_0000), nz(0x4000), AccessMask::RW);
    assert_eq!(region.profile(), MemoryProfile::Device);
    let sub = region
        .slice(0x1000, nz(0x1000), AccessMask::R, SliceProfile::Inherited)
        .expect("in-bounds slice with subset access");
    assert_eq!(sub.kind_tag(), 2);
    assert_eq!(sub.size_bytes().get(), 0x1000);
    assert_eq!(sub.access_mask().bits(), AccessMask::R.bits());
    assert_eq!(sub.profile(), MemoryProfile::Device);
    assert_eq!(sub.physical_base(), Some(aligned_pa(0x4000_1000)));
}

#[test]
fn create_physical_normal_is_cached_profile() {
    let region =
        MemoryRegion::create_physical_normal(aligned_pa(0x4000_0000), nz(0x1000), AccessMask::R);
    assert_eq!(region.profile(), MemoryProfile::Cached);
}

#[test]
fn create_physical_device_stays_device_profile() {
    let region =
        MemoryRegion::create_physical_device(aligned_pa(0x4000_0000), nz(0x1000), AccessMask::R);
    assert_eq!(region.profile(), MemoryProfile::Device);
}

#[test]
fn slice_physical_normal_preserves_cached_profile() {
    let region =
        MemoryRegion::create_physical_normal(aligned_pa(0x4000_0000), nz(0x2000), AccessMask::R);
    let sub = region
        .slice(0x1000, nz(0x1000), AccessMask::R, SliceProfile::Inherited)
        .expect("in-bounds slice");
    assert_eq!(sub.profile(), MemoryProfile::Cached);
}

#[test]
fn write_combining_slice_retypes_a_device_window() {
    let device =
        MemoryRegion::create_physical_device(aligned_pa(0x4000_0000), nz(0x2000), AccessMask::RW);

    let combining = device
        .slice(0, nz(0x2000), AccessMask::RW, SliceProfile::WriteCombining)
        .expect("device window accepts write-combining");
    let nested = combining
        .slice(
            0x1000,
            nz(0x1000),
            AccessMask::R,
            SliceProfile::WriteCombining,
        )
        .expect("write-combining window accepts write-combining");

    assert_eq!(combining.profile(), MemoryProfile::WriteCombining);
    assert_eq!(nested.profile(), MemoryProfile::WriteCombining);
}

#[test]
fn write_combining_slice_refuses_cacheable_sources() {
    let physical =
        MemoryRegion::create_physical_normal(aligned_pa(0x4000_0000), nz(PAGE_SIZE), AccessMask::R);
    let virt = MemoryRegion::create_virtual(leak_fa(), nz(1), AccessMask::R).unwrap();

    for source in [&physical, &virt] {
        assert!(matches!(
            source.slice(
                0,
                nz(PAGE_SIZE),
                AccessMask::R,
                SliceProfile::WriteCombining
            ),
            Err(RegionSliceError::CacheableSource)
        ));
    }
}

#[test]
fn install_physical_forwards_flags_to_map_exact() {
    let region =
        MemoryRegion::create_physical_normal(aligned_pa(0x8000_0000), nz(PAGE_SIZE), AccessMask::R);
    let mapper = PageMapper::new();
    region
        .install(&mapper, aligned_va(0x4000_0000), MemFlags::user_ro())
        .expect("install must succeed");
    let calls = mapper.calls();
    assert_eq!(calls.len(), 1);
    assert!(matches!(
        calls[0],
        Call::MapExact(MapExactCall {
            flags: MemFlags::Cached(_),
            ..
        })
    ));
}

#[test]
fn virtual_region_is_cached_profile() {
    let fa = Box::leak(Box::new(MockFrameAllocator::with_limit(1)));
    let region = MemoryRegion::create_virtual(fa, nz(1), AccessMask::RW).expect("alloc");
    assert_eq!(region.profile(), MemoryProfile::Cached);
}

#[test]
fn slice_rejects_window_past_region_end() {
    let region =
        MemoryRegion::create_physical_device(aligned_pa(0x4000_0000), nz(0x4000), AccessMask::RW);
    assert!(matches!(
        region.slice(0x3000, nz(0x2000), AccessMask::R, SliceProfile::Inherited),
        Err(RegionSliceError::OutOfBounds)
    ));
}

#[test]
fn slice_rejects_access_wider_than_region() {
    let region =
        MemoryRegion::create_physical_device(aligned_pa(0x4000_0000), nz(0x4000), AccessMask::R);
    assert!(matches!(
        region.slice(0, nz(0x1000), AccessMask::RW, SliceProfile::Inherited),
        Err(RegionSliceError::AccessEscalation)
    ));
}

#[test]
fn slice_rejects_misaligned_offset() {
    let region =
        MemoryRegion::create_physical_device(aligned_pa(0x4000_0000), nz(0x4000), AccessMask::RW);
    assert!(matches!(
        region.slice(0x800, nz(0x1000), AccessMask::R, SliceProfile::Inherited),
        Err(RegionSliceError::MisalignedOffset)
    ));
}

#[test]
fn slice_of_virtual_maps_the_selected_frames() {
    let fa = leak_fa();
    let region = MemoryRegion::create_virtual(fa, nz(2), AccessMask::RW).unwrap();
    let frames = fa.allocated();
    let sub = region
        .slice(
            PAGE_SIZE,
            nz(PAGE_SIZE),
            AccessMask::R,
            SliceProfile::Inherited,
        )
        .expect("virtual slice");
    let mapper = PageMapper::new();
    sub.install(&mapper, aligned_va(0x4000_0000), MemFlags::user_ro())
        .expect("install slice");
    let entries = mapper.map_exacts();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].pa, frames[1].page_address().as_usize());
}

#[test]
fn slice_installs_at_offset_pa() {
    let region =
        MemoryRegion::create_physical_device(aligned_pa(0x8000_0000), nz(0x2000), AccessMask::RW);
    let sub = region
        .slice(0x1000, nz(0x1000), AccessMask::R, SliceProfile::Inherited)
        .unwrap();
    let mapper = PageMapper::new();
    let va = aligned_va(0x4000_0000);
    sub.install(&mapper, va, MemFlags::user_ro())
        .expect("install must succeed");
    let entries = mapper.map_exacts();
    assert_eq!(entries[0].pa, 0x8000_1000);
    assert_eq!(entries[0].size, 0x1000);
}

#[test]
fn physical_base_some_for_physical_none_for_virtual() {
    let phys =
        MemoryRegion::create_physical_device(aligned_pa(0x9010_0000), nz(0x1000), AccessMask::RW);
    assert_eq!(
        phys.physical_base().map(PageAlignedAddress::as_usize),
        Some(0x9010_0000)
    );

    let fa = leak_fa();
    let virt = MemoryRegion::create_virtual(fa, nz(1), AccessMask::RW).unwrap();
    assert_eq!(virt.physical_base(), None);
}

#[test]
fn contiguous_region_reports_the_physical_base_of_each_slice() {
    let fa: &'static PhysicalFrameAllocator<MutexCell<FrameBitmap>> = Box::leak(Box::new(
        PhysicalFrameAllocator::new([make_range(0, 16)].into_iter()).unwrap(),
    ));
    let region = MemoryRegion::create_contiguous(fa, nz(4), AccessMask::RW).unwrap();
    let sub = region
        .slice(
            PAGE_SIZE,
            nz(PAGE_SIZE),
            AccessMask::R,
            SliceProfile::Inherited,
        )
        .expect("contiguous slice");

    let base = region.physical_base().map(PageAlignedAddress::as_usize);
    let sub_base = sub.physical_base().map(PageAlignedAddress::as_usize);

    assert_eq!(region.kind_tag(), 1);
    assert!(base.is_some());
    assert_eq!(sub_base, base.map(|base| base + PAGE_SIZE));
}

#[test]
fn physical_drop_does_nothing() {
    let region =
        MemoryRegion::create_physical_device(aligned_pa(0x4000_0000), nz(PAGE_SIZE), AccessMask::R);
    assert_eq!(region.kind_tag(), 2);
    assert_eq!(region.size_bytes().get(), PAGE_SIZE);
    drop(region);
}

struct CountingRefund(AtomicUsize);

impl BudgetRefund for CountingRefund {
    fn refund(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[test]
fn metered_physical_drop_refunds_exactly_once() {
    let refund = Arc::new(CountingRefund(AtomicUsize::new(0)));
    let region =
        MemoryRegion::create_physical_device(aligned_pa(0x4000_0000), nz(PAGE_SIZE), AccessMask::R)
            .with_refund(refund.clone());
    assert_eq!(refund.0.load(Ordering::Acquire), 0);
    drop(region);
    assert_eq!(refund.0.load(Ordering::Acquire), 1);
}

#[test]
fn metered_virtual_drop_refunds_and_returns_frames() {
    let fa = leak_fa();
    let refund = Arc::new(CountingRefund(AtomicUsize::new(0)));
    let region = MemoryRegion::create_virtual(fa, nz(2), AccessMask::RW)
        .expect("alloc must succeed")
        .with_refund(refund.clone());
    assert_eq!(refund.0.load(Ordering::Acquire), 0);
    assert_eq!(fa.deallocated().len(), 0);

    drop(region);
    assert_eq!(refund.0.load(Ordering::Acquire), 1);
    assert_eq!(fa.deallocated().len(), 2);
}

#[test]
fn virtual_slice_keeps_frames_and_refund_alive() {
    let fa = leak_fa();
    let refund = Arc::new(CountingRefund(AtomicUsize::new(0)));
    let region = MemoryRegion::create_virtual(fa, nz(2), AccessMask::RW)
        .expect("alloc must succeed")
        .with_refund(refund.clone());
    let sub = region
        .slice(
            PAGE_SIZE,
            nz(PAGE_SIZE),
            AccessMask::R,
            SliceProfile::Inherited,
        )
        .expect("virtual slice");

    drop(region);
    assert_eq!(refund.0.load(Ordering::Acquire), 0);
    assert_eq!(fa.deallocated().len(), 0);
    drop(sub);
    assert_eq!(refund.0.load(Ordering::Acquire), 1);
    assert_eq!(fa.deallocated().len(), 2);
}

#[test]
fn install_uninstall_round_trip_virtual() {
    let fa = leak_fa();
    let region =
        MemoryRegion::create_virtual(fa, nz(2), AccessMask::RW).expect("alloc must succeed");
    let mapper = PageMapper::new();
    let va = aligned_va(0x4000_0000);
    region
        .install(&mapper, va, MemFlags::user_rw())
        .expect("install must succeed");
    assert_eq!(mapper.map_exacts().len(), 2);
    region
        .uninstall(&mapper, va)
        .expect("uninstall must succeed");
    let unmapped = mapper.unmaps();
    assert_eq!(
        unmapped,
        [UnmapCall {
            va: va.as_usize(),
            size: 2 * PAGE_SIZE
        }]
    );
}

#[test]
fn install_uninstall_round_trip_physical() {
    let region =
        MemoryRegion::create_physical_device(aligned_pa(0x8000_0000), nz(PAGE_SIZE), AccessMask::R);
    let mapper = PageMapper::new();
    let va = aligned_va(0x4000_0000);
    region
        .install(&mapper, va, MemFlags::user_ro())
        .expect("install must succeed");
    let entries = mapper.map_exacts();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].va, va.as_usize());
    assert_eq!(entries[0].pa, 0x8000_0000);
    assert_eq!(entries[0].size, PAGE_SIZE);
    region
        .uninstall(&mapper, va)
        .expect("uninstall must succeed");
}

#[test]
fn install_rolls_back_on_failure() {
    let fa = leak_fa();
    let region =
        MemoryRegion::create_virtual(fa, nz(4), AccessMask::RW).expect("alloc must succeed");
    let mapper = PageMapper::new();
    mapper.fail_map_exact_after(2);
    let va = aligned_va(0x5000_0000);
    let err = region.install(&mapper, va, MemFlags::user_rw());
    assert!(err.is_err());
    assert_eq!(err.err().unwrap(), MemoryMappingError::OutOfMemory);
    let unmapped = mapper.unmaps();
    assert_eq!(unmapped.len(), 2);
    assert_eq!(unmapped[0].va, va.as_usize());
    assert_eq!(unmapped[1].va, va.as_usize() + PAGE_SIZE);
}

#[test]
fn access_mask_preserved() {
    let fa = leak_fa();
    let region = MemoryRegion::create_virtual(fa, nz(1), AccessMask::RX).unwrap();
    assert_eq!(region.access_mask().bits(), AccessMask::RX.bits());
}

#[test]
fn install_zeroes_each_frame_on_first_install() {
    let fa = leak_fa();
    let region = MemoryRegion::create_virtual(fa, nz(3), AccessMask::RW).unwrap();
    let mapper = PageMapper::new();
    let va = aligned_va(0x4000_0000);
    region
        .install(&mapper, va, MemFlags::user_rw())
        .expect("install must succeed");
    assert_eq!(mapper.zeroed_frames().len(), 3);
}

#[test]
fn install_after_uninstall_does_not_rezero() {
    let fa = leak_fa();
    let region = MemoryRegion::create_virtual(fa, nz(2), AccessMask::RW).unwrap();
    let mapper = PageMapper::new();
    let va = aligned_va(0x4000_0000);
    region.install(&mapper, va, MemFlags::user_rw()).unwrap();
    region.uninstall(&mapper, va).unwrap();
    region.install(&mapper, va, MemFlags::user_rw()).unwrap();
    assert_eq!(mapper.zeroed_frames().len(), 2);
}

#[test]
fn install_zeroes_before_mapping_on_partial_oom() {
    let fa = leak_fa();
    let region = MemoryRegion::create_virtual(fa, nz(4), AccessMask::RW).unwrap();
    let mapper = PageMapper::new();
    mapper.fail_map_exact_after(2);
    let va = aligned_va(0x5000_0000);
    let _ = region.install(&mapper, va, MemFlags::user_rw());
    assert_eq!(mapper.zeroed_frames().len(), 4);

    let mapper2 = PageMapper::new();
    region.install(&mapper2, va, MemFlags::user_rw()).unwrap();
    assert_eq!(mapper2.zeroed_frames().len(), 0);
}

#[test]
fn install_physical_does_not_zero() {
    let region =
        MemoryRegion::create_physical_device(aligned_pa(0x8000_0000), nz(PAGE_SIZE), AccessMask::R);
    let mapper = PageMapper::new();
    region
        .install(&mapper, aligned_va(0x4000_0000), MemFlags::user_ro())
        .unwrap();
    assert_eq!(mapper.zeroed_frames().len(), 0);
}

#[test]
fn install_physical_sub_page_region_succeeds() {
    let region =
        MemoryRegion::create_physical_device(aligned_pa(0x8000_0000), nz(0x200), AccessMask::R);
    let mapper = PageMapper::new();
    region
        .install(&mapper, aligned_va(0x4000_0000), MemFlags::user_ro())
        .expect("sub-page physical region installs");

    let entries = mapper.map_exacts();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].size, 0x200);
}

#[test]
fn uninstall_physical_sub_page_region_unmaps_the_whole_page() {
    let region =
        MemoryRegion::create_physical_device(aligned_pa(0x8000_0000), nz(0x200), AccessMask::R);
    let mapper = PageMapper::new();
    let va = aligned_va(0x4000_0000);
    region.install(&mapper, va, MemFlags::user_ro()).unwrap();

    region
        .uninstall(&mapper, va)
        .expect("sub-page physical region uninstalls");

    assert_eq!(
        mapper.unmaps(),
        [UnmapCall {
            va: va.as_usize(),
            size: PAGE_SIZE
        }]
    );
}
