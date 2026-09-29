//! VM-rollback: `region.install`-ошибка в `sys_memory_allocate`/`sys_memory_map`
//! обязана откатить VA и освободить фреймы (alloc == dealloc).
//! Проверяется через spy-FrameAllocator.

#![allow(clippy::unwrap_used)]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unimplemented,
    clippy::unwrap_in_result
)]

mod common;

use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::sync::{Arc, OnceLock};

use capability::{Capability, HandleId, HandleTable, Resource};
use collections::{LockCell, MutexCell};
use memory::{
    AccessMask, MemoryRegion,
    frame::Frame,
    frame_allocator::{FrameAllocator, FrameError, ReserveFrameError},
    physical_address::PageAlignedAddress,
    range_allocator::RangeError,
    testing::PageMapper,
    user_vm_allocator::UserVmAllocator,
    virtual_address::PageAlignedVirtualAddress,
};
use syscall_kernel::{SyscallError, SyscallOp};

use crate::common::{USER_VM_ARENA_BASE, dispatch, runtimes, test_lock, user_vm_allocator};

const PAGE: usize = memory::PAGE_SIZE.get();

struct SpyFrameAllocator {
    next: AtomicUsize,
    allocated: AtomicUsize,
    deallocated: AtomicUsize,
    blocked: AtomicBool,
}

impl SpyFrameAllocator {
    fn new() -> Self {
        Self {
            next: AtomicUsize::new(100),
            allocated: AtomicUsize::new(0),
            deallocated: AtomicUsize::new(0),
            blocked: AtomicBool::new(false),
        }
    }
    fn outstanding(&self) -> i64 {
        let allocated = i64::try_from(self.allocated.load(Ordering::SeqCst)).unwrap();
        let deallocated = i64::try_from(self.deallocated.load(Ordering::SeqCst)).unwrap();
        allocated - deallocated
    }
    fn set_blocked(&self, blocked: bool) {
        self.blocked.store(blocked, Ordering::SeqCst);
    }
}

impl FrameAllocator for SpyFrameAllocator {
    fn reserve_frames_exact(&self, _from: Frame, _to: Frame) -> Result<Frame, ReserveFrameError> {
        unimplemented!()
    }
    fn allocate_frame(&self) -> Option<Frame> {
        if self.blocked.load(Ordering::SeqCst) {
            return None;
        }
        self.allocated.fetch_add(1, Ordering::SeqCst);
        Some(Frame::new(self.next.fetch_add(1, Ordering::SeqCst)))
    }
    fn allocate_frames(&self, _max_count: usize) -> Option<(Frame, usize)> {
        unimplemented!()
    }
    fn deallocate_frame(&self, _frame: Frame) -> Result<(), FrameError> {
        self.deallocated.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn is_allocated(&self, _frame: Frame) -> bool {
        true
    }
}

type TestEnv = (
    Arc<MutexCell<HandleTable>>,
    Arc<MutexCell<UserVmAllocator>>,
    &'static SpyFrameAllocator,
);

fn setup() -> TestEnv {
    static STATE: OnceLock<(&'static SpyFrameAllocator, Arc<MutexCell<UserVmAllocator>>)> =
        OnceLock::new();
    let (fa, allocator) = STATE.get_or_init(|| {
        let fa: &'static SpyFrameAllocator = Box::leak(Box::new(SpyFrameAllocator::new()));
        let (_, stub) = runtimes();
        let allocator = user_vm_allocator();
        stub.set_frame_allocator(fa);
        stub.set_user_vm(failing_mapper(), allocator.clone());
        (fa, allocator)
    });
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    runtimes().0.set_handle_table(table.clone());
    (table, allocator.clone(), fa)
}

fn insert_metering_resource(table: &Arc<MutexCell<HandleTable>>) -> (u64, Arc<Resource>) {
    let resource = Resource::new(
        PageAlignedAddress::ZERO,
        NonZeroUsize::new(usize::MAX & !0xFFF).unwrap(),
        AccessMask::RW,
        1024,
    );
    let id = table
        .with_lock(|tbl| tbl.insert(Capability::new_with_default_rights(resource.clone())))
        .expect("insert resource");
    (u64::from(id.raw().get()), resource)
}

#[test]
fn memory_allocate_rolls_back_va_and_frees_frames_on_install_failure() {
    let _g = test_lock();
    let (table, allocator, fa) = setup();
    let (resource_h, resource) = insert_metering_resource(&table);
    let before = fa.outstanding();
    let budget_before = resource.remaining_budget();

    let res = dispatch(
        SyscallOp::MemoryAllocate,
        [resource_h, PAGE as u64, 0, 0, 0, 0],
    );
    assert_eq!(res, Err(SyscallError::OutOfMemory.as_return_value()));

    // rollback дропает Arc<MemoryRegion>, возвращая все фреймы.
    assert_eq!(
        fa.outstanding(),
        before,
        "rollback must free every frame allocated for the region"
    );
    assert_eq!(
        resource.remaining_budget(),
        budget_before,
        "rollback must refund the metered budget"
    );

    let base = PageAlignedVirtualAddress::from_usize(USER_VM_ARENA_BASE).unwrap();
    let size = NonZeroUsize::new(PAGE).unwrap();
    let err = allocator.with_lock(|alloc| alloc.lookup(base, size).map(|_| ()));
    assert_eq!(
        err,
        Err(RangeError::NotFound),
        "VA must be freed on rollback"
    );
}

// Бюджет не должен утечь при frame-OOM до создания региона: refund висит на
// регионе, а регион при этом ещё не создан - спишутся страницы без возврата.
#[test]
fn metered_alloc_does_not_leak_budget_on_frame_oom() {
    let _g = test_lock();
    let (table, _allocator, fa) = setup();
    let (resource_h, resource) = insert_metering_resource(&table);
    let budget_before = resource.remaining_budget();

    fa.set_blocked(true);
    let alloc = dispatch(
        SyscallOp::MemoryAllocate,
        [resource_h, PAGE as u64, 0, 0, 0, 0],
    );
    let create = dispatch(
        SyscallOp::MemoryCreateVirtual,
        [
            resource_h,
            PAGE as u64,
            u64::from(AccessMask::RW.bits()),
            0,
            0,
            0,
        ],
    );
    fa.set_blocked(false);

    assert_eq!(alloc, Err(SyscallError::OutOfMemory.as_return_value()));
    assert_eq!(create, Err(SyscallError::OutOfMemory.as_return_value()));
    assert_eq!(
        resource.remaining_budget(),
        budget_before,
        "frame-OOM before region creation must not consume budget"
    );
}

#[test]
fn memory_map_rolls_back_va_and_keeps_region_frames_on_install_failure() {
    let _g = test_lock();
    let (table, allocator, fa) = setup();

    let region = Arc::new(
        MemoryRegion::create_virtual(
            *fa_static(fa),
            NonZeroUsize::new(1).unwrap(),
            AccessMask::RW,
        )
        .expect("region create"),
    );
    let frames_after_create = fa.outstanding();
    let region_id: HandleId = table
        .with_lock(|tbl| tbl.insert(Capability::new_with_default_rights(region)))
        .expect("insert region");

    let res = dispatch(
        SyscallOp::MemoryMap,
        [u64::from(region_id.raw().get()), PAGE as u64, 0, 0, 0, 0],
    );
    assert_eq!(res, Err(SyscallError::OutOfMemory.as_return_value()));

    // Регион всё ещё жив в таблице, поэтому его фреймы не освобождаются;
    // освобождается только VA-диапазон.
    assert_eq!(
        fa.outstanding(),
        frames_after_create,
        "map rollback must not free the externally-owned region's frames"
    );
    let base = PageAlignedVirtualAddress::from_usize(USER_VM_ARENA_BASE).unwrap();
    let size = NonZeroUsize::new(PAGE).unwrap();
    let err = allocator.with_lock(|alloc| alloc.lookup(base, size).map(|_| ()));
    assert_eq!(
        err,
        Err(RangeError::NotFound),
        "VA must be freed on rollback"
    );
}

fn fa_static(
    fa: &'static SpyFrameAllocator,
) -> &'static &'static (dyn FrameAllocator + Send + Sync) {
    Box::leak(Box::new(fa as &'static (dyn FrameAllocator + Send + Sync)))
}

fn failing_mapper() -> Arc<PageMapper> {
    let mapper = PageMapper::new();
    mapper.fail_map_after(0);
    mapper.fail_map_exact_after(0);
    Arc::new(mapper)
}
