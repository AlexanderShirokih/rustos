//! При `OutOfHandles` в caller-table syscall'ы `ProcessCreate`,
//! `ThreadCreate`, `ProcessStart` НЕ должны выполнять сторонних эффектов
//! (создавать процесс/поток, drain'ить bootstrap-handles).

use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};
use std::sync::{Arc, Mutex, OnceLock};

use collections::{LockCell, MutexCell};
use kobject::{
    Handle, HandleTable, IpcError, KObject, KernelRuntime, LoadImageError, ProcessObject, Rights,
    Signal, SpawnError, StartProcessError, ThreadObject, UserImageInstall, UserStartSpec,
    UserThreadEntry, WaitToken, install_runtime,
};
use memory::{
    MemFlags, UserVmContext,
    frame_allocator::FrameAllocator,
    memory_mapper::{
        AddressSpaceHandle, AddressSpaceTag, MemoryMapper, MemoryMappingError,
        MemoryRemappingError, MemoryUnmappingError, UserCopyError,
    },
    physical_address::{PageAlignedAddress, PhysicalAddress},
    user_vm_allocator::UserVmAllocator,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use syscall_kernel::{Origin, SyscallError, SyscallFrame, SyscallOp};

struct CountingRuntime {
    handle_table: Mutex<Option<Arc<MutexCell<HandleTable>>>>,
}

impl CountingRuntime {
    fn new() -> Self {
        Self {
            handle_table: Mutex::new(None),
        }
    }

    fn set_handle_table(&self, table: Arc<MutexCell<HandleTable>>) {
        *self.handle_table.lock().unwrap() = Some(table);
    }
}

impl KernelRuntime for CountingRuntime {
    fn current_wait_token(&self) -> WaitToken {
        WaitToken::new(NonZeroU64::new(1).unwrap())
    }

    fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
        self.handle_table.lock().unwrap().clone()
    }

    fn exit_current_thread(&self, _exit_code: i32) -> ! {
        panic!("exit_current_thread must not be called in this test");
    }

    fn block_current_until(&self, _ready_flag: &AtomicU32, _timeout_ns: Option<u64>) {}

    fn unblock(&self, _token: WaitToken) {}

    fn set_blocked_cancel(&self, _cancel: Arc<dyn kobject::CancelTarget>) {}

    fn clear_blocked_cancel(&self) {}
}

/// Минимальный mapper, обслуживающий только `copy_user_in` поверх Vec<u8>;
/// все остальные методы паникуют, потому что в этих тестах не вызываются.
struct CannedMapper {
    base_va: usize,
    bytes: Mutex<std::vec::Vec<u8>>,
}

impl CannedMapper {
    fn new(base_va: usize, bytes: std::vec::Vec<u8>) -> Self {
        Self {
            base_va,
            bytes: Mutex::new(bytes),
        }
    }
}

impl MemoryMapper for CannedMapper {
    fn map(
        &self,
        _va: PageAlignedVirtualAddress,
        _page_count: usize,
        _init: &[u8],
        _flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        unimplemented!("CannedMapper supports only copy_user_in")
    }

    fn map_exact(
        &self,
        _va: PageAlignedVirtualAddress,
        _pa: PageAlignedAddress,
        _size: usize,
        _flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        unimplemented!("CannedMapper supports only copy_user_in")
    }

    fn unmap(
        &self,
        _va: PageAlignedVirtualAddress,
        _size: usize,
    ) -> Result<(), MemoryUnmappingError> {
        unimplemented!("CannedMapper supports only copy_user_in")
    }

    fn remap(
        &self,
        _va: PageAlignedVirtualAddress,
        _size: usize,
        _flags: MemFlags,
    ) -> Result<(), MemoryRemappingError> {
        unimplemented!("CannedMapper supports only copy_user_in")
    }

    fn activate_handle(&self) -> AddressSpaceHandle {
        AddressSpaceHandle::new(PhysicalAddress::new(0), AddressSpaceTag::NONE)
    }

    fn zero_owned_frame(&self, _pa: PageAlignedAddress) {
        unimplemented!("CannedMapper supports only copy_user_in")
    }

    fn copy_user_in(&self, va: VirtualAddress, dst: &mut [u8]) -> Result<(), UserCopyError> {
        let bytes = self.bytes.lock().unwrap();
        let offset = va.as_usize().wrapping_sub(self.base_va);
        let end = offset
            .checked_add(dst.len())
            .ok_or(UserCopyError::NotMapped)?;
        if end > bytes.len() {
            return Err(UserCopyError::NotMapped);
        }
        dst.copy_from_slice(&bytes[offset..end]);
        Ok(())
    }

    fn as_any(&self) -> &(dyn core::any::Any + 'static) {
        self
    }
}

type CannedUserVm = (Arc<CannedMapper>, Arc<MutexCell<UserVmAllocator>>);

struct StubSyscallRuntime {
    user_vm: Mutex<Option<CannedUserVm>>,
    create_empty_process_calls: AtomicUsize,
    create_user_thread_calls: AtomicUsize,
    start_user_process_calls: AtomicUsize,
}

impl StubSyscallRuntime {
    fn new() -> Self {
        Self {
            user_vm: Mutex::new(None),
            create_empty_process_calls: AtomicUsize::new(0),
            create_user_thread_calls: AtomicUsize::new(0),
            start_user_process_calls: AtomicUsize::new(0),
        }
    }

    fn set_canned_user_vm(&self, base_va: usize, bytes: std::vec::Vec<u8>) {
        let mapper = Arc::new(CannedMapper::new(base_va, bytes));
        let allocator = Arc::new(MutexCell::new(UserVmAllocator::new(
            PageAlignedVirtualAddress::from_usize(0x1000_0000).unwrap(),
            VirtualAddress::new(0x1100_0000),
        )));
        *self.user_vm.lock().unwrap() = Some((mapper, allocator));
    }

    fn clear_user_vm(&self) {
        *self.user_vm.lock().unwrap() = None;
    }

    fn reset_counters(&self) {
        self.create_empty_process_calls.store(0, Ordering::SeqCst);
        self.create_user_thread_calls.store(0, Ordering::SeqCst);
        self.start_user_process_calls.store(0, Ordering::SeqCst);
    }
}

impl syscall_kernel::SyscallRuntime for StubSyscallRuntime {
    fn current_user_vm(&self) -> Option<UserVmContext> {
        let guard = self.user_vm.lock().unwrap();
        let (mapper, allocator) = guard.as_ref()?;
        Some(UserVmContext::new(mapper.clone(), allocator.clone()))
    }

    fn current_ipc_buffer_va(&self) -> Option<u64> {
        None
    }

    fn frame_allocator(&self) -> Option<&'static (dyn FrameAllocator + Send + Sync)> {
        None
    }

    fn current_thread_object(&self) -> Option<Arc<ThreadObject>> {
        None
    }

    fn current_process_object(&self) -> Option<Arc<ProcessObject>> {
        None
    }

    fn create_empty_process(&self, name: &str) -> Result<Arc<ProcessObject>, SpawnError> {
        self.create_empty_process_calls
            .fetch_add(1, Ordering::SeqCst);
        if name.is_empty() {
            return Err(SpawnError::InvalidName);
        }
        Ok(ProcessObject::new())
    }

    fn create_user_thread(
        &self,
        _process: &Arc<ProcessObject>,
        _entry: UserThreadEntry,
    ) -> Result<Arc<ThreadObject>, SpawnError> {
        self.create_user_thread_calls.fetch_add(1, Ordering::SeqCst);
        Ok(ThreadObject::new())
    }

    fn terminate_thread(
        &self,
        _thread: &Arc<ThreadObject>,
        _exit_code: i32,
    ) -> Result<(), IpcError> {
        Ok(())
    }

    fn terminate_process(
        &self,
        _process: &Arc<ProcessObject>,
        _exit_code: i32,
    ) -> Result<(), IpcError> {
        Ok(())
    }

    fn load_user_image_into(
        &self,
        _process: &Arc<ProcessObject>,
        _install: &UserImageInstall,
    ) -> Result<(), LoadImageError> {
        Ok(())
    }

    fn start_user_process(
        &self,
        _process: &Arc<ProcessObject>,
        spec: UserStartSpec,
    ) -> Result<Arc<ThreadObject>, StartProcessError> {
        self.start_user_process_calls.fetch_add(1, Ordering::SeqCst);
        // Моделируем реальный drain bootstrap-handle'ов: фиксу важна именно
        // та инвариантa, что drain освобождает слоты в caller-table до
        // пост-резервации возвращаемого thread-handle'а.
        spec.loader_handle_table
            .with_lock(|tbl| tbl.try_drain_for_transfer(&spec.handle_ids, Rights::TRANSFER))
            .map_err(StartProcessError::HandleValidationFailed)?;
        Ok(ThreadObject::new())
    }
}

fn shared_runtime() -> &'static (Arc<CountingRuntime>, Arc<StubSyscallRuntime>) {
    static RUNTIME: OnceLock<(Arc<CountingRuntime>, Arc<StubSyscallRuntime>)> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let rt = Arc::new(CountingRuntime::new());
        let dyn_rt: Arc<dyn KernelRuntime> = rt.clone();
        install_runtime(dyn_rt);
        let stub = Arc::new(StubSyscallRuntime::new());
        syscall_kernel::install_runtime(stub.clone());
        (rt, stub)
    })
}

fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn full_table_with_capacity_one() -> Arc<MutexCell<HandleTable>> {
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(1)));
    let dummy = Handle::new(KObject::Signal(Signal::new()), Rights::READ);
    table
        .with_lock(|tbl| tbl.insert(dummy))
        .expect("dummy fills the slot");
    table
}

fn install_process_handle(table: &Arc<MutexCell<HandleTable>>) -> kobject::HandleId {
    let process = ProcessObject::new();
    let handle = Handle::new(KObject::Process(process), Rights::WRITE);
    table
        .with_lock(|tbl| tbl.insert(handle))
        .expect("insert process handle")
}

struct TestFrame {
    op: u16,
    args: [u64; 6],
    ret: i64,
    secondary: u64,
}

impl TestFrame {
    fn new(op: SyscallOp, args: [u64; 6]) -> Self {
        Self {
            op: op as u16,
            args,
            ret: 0,
            secondary: 0,
        }
    }
}

impl SyscallFrame for TestFrame {
    fn op_raw(&self) -> u16 {
        self.op
    }

    fn arg(&self, idx: usize) -> u64 {
        self.args[idx]
    }

    fn set_return(&mut self, value: i64) {
        self.ret = value;
    }

    fn set_secondary_return(&mut self, value: u64) {
        self.secondary = value;
    }

    fn origin(&self) -> Origin {
        Origin::User
    }
}

fn dispatch(op: SyscallOp, args: [u64; 6]) -> Result<u64, SyscallError> {
    let mut frame = TestFrame::new(op, args);
    syscall_kernel::dispatch(&mut frame);
    if frame.ret >= 0 {
        Ok(u64::try_from(frame.ret).expect("non-negative return fits in u64"))
    } else {
        let neg = u32::try_from(-frame.ret).expect("error code fits in u32");
        Err(decode_error(neg))
    }
}

/// Декодирует через `as_return_value`, не через хардкод, чтобы новые коды не давали панику.
const ALL_ERRORS: &[SyscallError] = &[
    SyscallError::BadSyscall,
    SyscallError::KernelOriginated,
    SyscallError::InvalidArgument,
    SyscallError::BadHandle,
    SyscallError::WrongType,
    SyscallError::AccessDenied,
    SyscallError::ShouldWait,
    SyscallError::PeerClosed,
    SyscallError::Timeout,
    SyscallError::BufferTooSmall,
    SyscallError::MessageTooBig,
    SyscallError::OutOfHandles,
    SyscallError::OutOfMemory,
    SyscallError::NotFound,
    SyscallError::Canceled,
    SyscallError::ResourceExhausted,
];

fn decode_error(code: u32) -> SyscallError {
    let want = -i64::from(code);
    *ALL_ERRORS
        .iter()
        .find(|e| e.as_return_value() == want)
        .unwrap_or_else(|| panic!("unknown SyscallError code: {code}"))
}

#[test]
fn decode_error_handles_all_abi_codes_including_resource_exhausted() {
    for &e in ALL_ERRORS {
        let code = u32::try_from(-e.as_return_value()).expect("error code fits u32");
        assert_eq!(decode_error(code), e);
    }
    assert_eq!(decode_error(16), SyscallError::ResourceExhausted);
}

#[test]
fn process_create_does_not_create_process_on_out_of_handles() {
    let _guard = test_lock();
    let (rt, stub) = shared_runtime();
    stub.reset_counters();
    stub.clear_user_vm();

    rt.set_handle_table(full_table_with_capacity_one());

    let err =
        dispatch(SyscallOp::ProcessCreate, [0, 0, 0, 0, 0, 0]).expect_err("OutOfHandles expected");
    assert_eq!(err, SyscallError::OutOfHandles);
    assert_eq!(
        stub.create_empty_process_calls.load(Ordering::SeqCst),
        0,
        "create_empty_process must NOT be called on OutOfHandles",
    );
}

#[test]
fn process_create_with_empty_name_returns_invalid_argument() {
    let _guard = test_lock();
    let (rt, stub) = shared_runtime();
    stub.reset_counters();
    stub.clear_user_vm();

    rt.set_handle_table(Arc::new(MutexCell::new(HandleTable::with_capacity(2))));

    let err = dispatch(SyscallOp::ProcessCreate, [0, 0, 0, 0, 0, 0])
        .expect_err("InvalidArgument expected");
    assert_eq!(err, SyscallError::InvalidArgument);
    assert_eq!(
        stub.create_empty_process_calls.load(Ordering::SeqCst),
        1,
        "create_empty_process must validate the empty name",
    );
}

#[test]
fn thread_create_does_not_create_thread_on_out_of_handles() {
    let _guard = test_lock();
    let (rt, stub) = shared_runtime();
    stub.reset_counters();
    stub.clear_user_vm();

    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(2)));
    let process_id = install_process_handle(&table);
    table
        .with_lock(|tbl| {
            tbl.insert(Handle::new(KObject::Signal(Signal::new()), Rights::READ))
                .map(|_| ())
        })
        .expect("dummy fills second slot");
    rt.set_handle_table(table);

    let err = dispatch(
        SyscallOp::ThreadCreate,
        [
            u64::from(process_id.raw().get()),
            0x4000_0000,
            0x4001_0000,
            0,
            1,
            0,
        ],
    )
    .expect_err("OutOfHandles expected");
    assert_eq!(err, SyscallError::OutOfHandles);
    assert_eq!(
        stub.create_user_thread_calls.load(Ordering::SeqCst),
        0,
        "create_user_thread must NOT be called on OutOfHandles",
    );
}

#[test]
fn process_start_does_not_start_process_on_out_of_handles() {
    let _guard = test_lock();
    let (rt, stub) = shared_runtime();
    stub.reset_counters();
    stub.clear_user_vm();

    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(2)));
    let process_id = install_process_handle(&table);
    table
        .with_lock(|tbl| {
            tbl.insert(Handle::new(KObject::Signal(Signal::new()), Rights::READ))
                .map(|_| ())
        })
        .expect("dummy fills second slot");
    rt.set_handle_table(table);

    let prio_and_count = 1u64;
    let err = dispatch(
        SyscallOp::ProcessStart,
        [
            u64::from(process_id.raw().get()),
            0x4000_0000,
            0x4001_0000,
            0,
            prio_and_count,
            0,
        ],
    )
    .expect_err("OutOfHandles expected");
    assert_eq!(err, SyscallError::OutOfHandles);
    assert_eq!(
        stub.start_user_process_calls.load(Ordering::SeqCst),
        0,
        "start_user_process must NOT be called on OutOfHandles",
    );
}

#[test]
fn process_start_succeeds_when_drain_frees_caller_slot() {
    // Регрессия: при handles_count >= 1 пере-передача bootstrap-handle-а
    // освобождает слот в caller-table под возвращаемый thread-handle, и
    // syscall не должен отказывать пре-резервацией.
    let _guard = test_lock();
    let (rt, stub) = shared_runtime();
    stub.reset_counters();

    // capacity=2: свободных слотов нет; drain bootstrap-handle-а освободит ровно один.
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(2)));
    let process_id = install_process_handle(&table);
    let bootstrap_id = table
        .with_lock(|tbl| {
            tbl.insert(Handle::new(
                KObject::Signal(Signal::new()),
                Rights::READ | Rights::TRANSFER,
            ))
        })
        .expect("insert bootstrap handle");
    rt.set_handle_table(table.clone());

    let handles_va = 0x1000_0000u64;
    stub.set_canned_user_vm(
        handles_va as usize,
        bootstrap_id.raw().get().to_le_bytes().to_vec(),
    );

    let prio_and_count = 1u64 | (1u64 << 32);
    let ret = dispatch(
        SyscallOp::ProcessStart,
        [
            u64::from(process_id.raw().get()),
            0x4000_0000,
            0x4001_0000,
            0,
            prio_and_count,
            handles_va,
        ],
    )
    .expect("ProcessStart must succeed once drain frees the bootstrap slot");
    assert_ne!(ret, 0, "returned thread-handle must be non-zero");
    assert_eq!(stub.start_user_process_calls.load(Ordering::SeqCst), 1);
    table.with_lock(|tbl| {
        assert!(tbl.get(bootstrap_id, Rights::empty()).is_err());
        assert_eq!(tbl.live_count(), 2);
    });
}
