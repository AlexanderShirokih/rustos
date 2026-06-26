//! VM-rollback: `region.install`-ошибка в `sys_memory_allocate`/`sys_memory_map`
//! обязана откатить VA и освободить фреймы (alloc == dealloc).
//! Проверяется через spy-FrameAllocator.

use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
};
use std::sync::{Arc, Mutex, OnceLock};

use capability::{
    Capability, HandleId, HandleTable, IpcError, KernelRuntime, LoadImageError, ProcessObject,
    Resource, SpawnError, StartProcessError, ThreadObject, UserImageInstall, UserStartSpec,
    UserThreadEntry, WaitToken, install_runtime,
};
use collections::{LockCell, MutexCell};
use memory::{
    AccessMask, MemFlags, MemoryRegion, UserVmContext,
    frame::Frame,
    frame_allocator::{FrameAllocator, FrameError, ReserveFrameError},
    memory_mapper::{
        AddressSpaceHandle, AddressSpaceTag, MemoryMapper, MemoryMappingError,
        MemoryRemappingError, MemoryUnmappingError,
    },
    physical_address::{PageAlignedAddress, PhysicalAddress},
    range_allocator::RangeError,
    user_vm_allocator::UserVmAllocator,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use syscall_kernel::{Origin, SyscallError, SyscallFrame, SyscallOp};

const PAGE: usize = 4096;
const ARENA_BASE: usize = 0x1000_0000;
const ARENA_END: usize = 0x1100_0000;

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
    /// Симулирует исчерпание фреймов: `allocate_frame` отдаёт `None`.
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

struct FailingMapper;

impl MemoryMapper for FailingMapper {
    fn map(
        &self,
        _va: PageAlignedVirtualAddress,
        _pc: usize,
        _init: &[u8],
        _f: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        Err(MemoryMappingError::OutOfMemory)
    }
    fn map_exact(
        &self,
        _va: PageAlignedVirtualAddress,
        _pa: PageAlignedAddress,
        _s: usize,
        _f: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        Err(MemoryMappingError::OutOfMemory)
    }
    fn unmap(&self, _va: PageAlignedVirtualAddress, _s: usize) -> Result<(), MemoryUnmappingError> {
        Ok(())
    }
    fn remap(
        &self,
        _va: PageAlignedVirtualAddress,
        _s: usize,
        _f: MemFlags,
    ) -> Result<(), MemoryRemappingError> {
        Ok(())
    }
    fn activate_handle(&self) -> AddressSpaceHandle {
        AddressSpaceHandle::new(PhysicalAddress::new(0), AddressSpaceTag::NONE)
    }
    fn zero_owned_frame(&self, _pa: PageAlignedAddress) {}
    fn as_any(&self) -> &(dyn core::any::Any + 'static) {
        self
    }
}

struct TableRuntime {
    handle_table: Mutex<Option<Arc<MutexCell<HandleTable>>>>,
}

impl KernelRuntime for TableRuntime {
    fn current_wait_token(&self) -> WaitToken {
        WaitToken::new(core::num::NonZeroU64::new(1).unwrap())
    }
    fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
        self.handle_table.lock().unwrap().clone()
    }
    fn exit_current_thread(&self, _c: i32) -> ! {
        panic!()
    }
    fn block_current_until(&self, _r: &AtomicU32, _t: Option<u64>) {}
    fn unblock(&self, _t: WaitToken) {}
    fn set_blocked_cancel(&self, _cancel: Arc<dyn capability::CancelTarget>) {}
    fn clear_blocked_cancel(&self) {}
}

struct StubSyscallRuntime {
    allocator: Arc<MutexCell<UserVmAllocator>>,
    fa: &'static SpyFrameAllocator,
}

impl syscall_kernel::SyscallRuntime for StubSyscallRuntime {
    fn current_user_vm(&self) -> Option<UserVmContext> {
        Some(UserVmContext::new(
            Arc::new(FailingMapper),
            self.allocator.clone(),
        ))
    }
    fn current_ipc_buffer_va(&self) -> Option<u64> {
        None
    }
    fn frame_allocator(&self) -> Option<&'static (dyn FrameAllocator + Send + Sync)> {
        Some(self.fa)
    }
    fn current_thread_object(&self) -> Option<Arc<ThreadObject>> {
        None
    }
    fn current_process_object(&self) -> Option<Arc<ProcessObject>> {
        None
    }
    fn create_empty_process(&self, _n: &str) -> Result<Arc<ProcessObject>, SpawnError> {
        Ok(ProcessObject::new())
    }
    fn create_user_thread(
        &self,
        _p: &Arc<ProcessObject>,
        _e: UserThreadEntry,
    ) -> Result<Arc<ThreadObject>, SpawnError> {
        Ok(ThreadObject::new())
    }
    fn terminate_thread(&self, _t: &Arc<ThreadObject>, _c: i32) -> Result<(), IpcError> {
        Ok(())
    }
    fn terminate_process(&self, _p: &Arc<ProcessObject>, _c: i32) -> Result<(), IpcError> {
        Ok(())
    }
    fn load_user_image_into(
        &self,
        _p: &Arc<ProcessObject>,
        _i: &UserImageInstall,
    ) -> Result<(), LoadImageError> {
        Ok(())
    }
    fn start_user_process(
        &self,
        _p: &Arc<ProcessObject>,
        _s: UserStartSpec,
    ) -> Result<Arc<ThreadObject>, StartProcessError> {
        Ok(ThreadObject::new())
    }
}

type SetupHandles = (
    &'static Arc<TableRuntime>,
    &'static SpyFrameAllocator,
    Arc<MutexCell<UserVmAllocator>>,
);

type TestEnv = (
    &'static Arc<TableRuntime>,
    Arc<MutexCell<HandleTable>>,
    Arc<MutexCell<UserVmAllocator>>,
    &'static SpyFrameAllocator,
);

fn setup() -> TestEnv {
    static R: OnceLock<SetupHandles> = OnceLock::new();
    let (rt, fa, allocator) = R.get_or_init(|| {
        let rt: &'static Arc<TableRuntime> = Box::leak(Box::new(Arc::new(TableRuntime {
            handle_table: Mutex::new(None),
        })));
        let concrete: Arc<TableRuntime> = (*rt).clone();
        let dyn_rt: Arc<dyn KernelRuntime> = concrete;
        install_runtime(dyn_rt);

        let fa: &'static SpyFrameAllocator = Box::leak(Box::new(SpyFrameAllocator::new()));
        let allocator = Arc::new(MutexCell::new(UserVmAllocator::new(
            PageAlignedVirtualAddress::from_usize(ARENA_BASE).unwrap(),
            VirtualAddress::new(ARENA_END),
        )));
        let stub: Arc<StubSyscallRuntime> = Arc::new(StubSyscallRuntime {
            allocator: allocator.clone(),
            fa,
        });
        syscall_kernel::install_runtime(stub);
        (rt, fa, allocator)
    });
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    *rt.handle_table.lock().unwrap() = Some(table.clone());
    (rt, table, allocator.clone(), fa)
}

/// Кладёт в таблицу метеринг-`Resource` с достаточным бюджетом и возвращает
/// raw `HandleId` для передачи как `resource_h` в memory-syscall'ы.
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

fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct TestFrame {
    op: u16,
    args: [u64; 6],
    ret: i64,
}
impl SyscallFrame for TestFrame {
    fn op_raw(&self) -> u16 {
        self.op
    }
    fn arg(&self, i: usize) -> u64 {
        self.args[i]
    }
    fn set_return(&mut self, v: i64) {
        self.ret = v;
    }
    fn set_secondary_return(&mut self, _v: u64) {}
    fn origin(&self) -> Origin {
        Origin::User
    }
}

fn dispatch(op: SyscallOp, args: [u64; 6]) -> Result<u64, i64> {
    let mut frame = TestFrame {
        op: op as u16,
        args,
        ret: 0,
    };
    syscall_kernel::dispatch(&mut frame);
    if frame.ret >= 0 {
        Ok(u64::try_from(frame.ret).unwrap())
    } else {
        Err(frame.ret)
    }
}

#[test]
fn memory_allocate_rolls_back_va_and_frees_frames_on_install_failure() {
    let _g = test_lock();
    let (_rt, table, allocator, fa) = setup();
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

    let base = PageAlignedVirtualAddress::from_usize(ARENA_BASE).unwrap();
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
    let (_rt, table, _allocator, fa) = setup();
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
    let (_rt, table, allocator, fa) = setup();

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

    // Регион всё ещё жив в таблице, поэтому его фреймы НЕ освобождаются;
    // освобождается только VA-диапазон.
    assert_eq!(
        fa.outstanding(),
        frames_after_create,
        "map rollback must not free the externally-owned region's frames"
    );
    let base = PageAlignedVirtualAddress::from_usize(ARENA_BASE).unwrap();
    let size = NonZeroUsize::new(PAGE).unwrap();
    let err = allocator.with_lock(|alloc| alloc.lookup(base, size).map(|_| ()));
    assert_eq!(
        err,
        Err(RangeError::NotFound),
        "VA must be freed on rollback"
    );
}

/// `MemoryRegion::create_virtual` требует `&'static dyn FrameAllocator`;
/// возвращаем именно такой reference из leaked spy-аллокатора.
fn fa_static(
    fa: &'static SpyFrameAllocator,
) -> &'static &'static (dyn FrameAllocator + Send + Sync) {
    Box::leak(Box::new(fa as &'static (dyn FrameAllocator + Send + Sync)))
}
