//! При `OutOfHandles` в caller-table syscall `ProcessCreate`,
//! `ThreadCreate`, `ProcessStart` не должны выполнять сторонних эффектов
//! (создавать процесс/поток, дренировать bootstrap-handle).

#![allow(clippy::unwrap_used)]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_in_result
)]

mod common;

use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use capability::{
    Capability, CapabilityTarget, DrainedHandles, HandleTable, IpcError, LoadImageError,
    ProcessObject, Rights, Signal, SpawnError, StartProcessError, ThreadObject, UserImageInstall,
    UserStartSpec, UserThreadEntry,
};
use kbase::{LockCell, MutexCell};
use memory::{MemFlags, UserVmContext, frame_allocator::FrameAllocator, testing::PageMapper};
use syscall_kernel::{SyscallError, SyscallOp};

use crate::common::{TableRuntime, install, test_lock, user_vm_allocator};

const NAME_VA: usize = 0x2000_0000;

struct StubSyscallRuntime {
    create_empty_process_calls: AtomicUsize,
    create_user_thread_calls: AtomicUsize,
    start_user_process_calls: AtomicUsize,
    user_vm: Mutex<Option<Arc<PageMapper>>>,
}

impl StubSyscallRuntime {
    fn new() -> Self {
        Self {
            create_empty_process_calls: AtomicUsize::new(0),
            create_user_thread_calls: AtomicUsize::new(0),
            start_user_process_calls: AtomicUsize::new(0),
            user_vm: Mutex::new(None),
        }
    }

    fn set_user_vm_name(&self, name: &[u8]) {
        *self.user_vm.lock().unwrap() =
            Some(PageMapper::with_mapping(NAME_VA, 1, name, MemFlags::user_ro()).unwrap());
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
        let mapper = self.user_vm.lock().unwrap().clone()?;
        Some(UserVmContext::new(mapper, user_vm_allocator()))
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

    fn thread_in_current_process(&self, _t: &Arc<ThreadObject>) -> Option<bool> {
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

        // Моделируем drain bootstrap-handle:
        let mut drained = DrainedHandles::empty();
        let taken = spec.loader_handle_table.with_lock(|tbl| {
            tbl.try_drain_for_transfer(&[spec.handle_id], Rights::TRANSFER, &mut drained)
        });
        drained.release();
        taken.map_err(StartProcessError::HandleValidationFailed)?;
        Ok(ThreadObject::new())
    }
}

fn shared_runtime() -> &'static (Arc<TableRuntime>, Arc<StubSyscallRuntime>) {
    static RUNTIME: OnceLock<(Arc<TableRuntime>, Arc<StubSyscallRuntime>)> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let stub = Arc::new(StubSyscallRuntime::new());
        (install(stub.clone()), stub)
    })
}

fn full_table_with_capacity_one() -> Arc<MutexCell<HandleTable>> {
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(1)));
    let dummy = Capability::new(CapabilityTarget::Signal(Signal::new()), Rights::READ);
    table
        .with_lock(|tbl| tbl.insert(dummy))
        .expect("dummy fills the slot");
    table
}

fn install_process_handle(table: &Arc<MutexCell<HandleTable>>) -> capability::HandleId {
    let process = ProcessObject::new();
    let handle = Capability::new(CapabilityTarget::Process(process), Rights::WRITE);
    table
        .with_lock(|tbl| tbl.insert(handle))
        .expect("insert process handle")
}

fn dispatch(op: SyscallOp, args: [u64; 6]) -> Result<u64, SyscallError> {
    common::dispatch(op, args)
        .map_err(|ret| decode_error(u32::try_from(-ret).expect("error code fits in u32")))
}

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
    stub.set_user_vm_name(b"x");

    rt.set_handle_table(full_table_with_capacity_one());

    let err = dispatch(SyscallOp::ProcessCreate, [NAME_VA as u64, 1, 0, 0, 0, 0])
        .expect_err("OutOfHandles expected");
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
        0,
        "syscall rejects the empty name before create_empty_process",
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
            tbl.insert(Capability::new(
                CapabilityTarget::Signal(Signal::new()),
                Rights::READ,
            ))
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
fn process_start_rejects_zero_handle() {
    let _guard = test_lock();
    let (rt, stub) = shared_runtime();
    stub.reset_counters();
    stub.clear_user_vm();

    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(2)));
    let process_id = install_process_handle(&table);
    table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Signal(Signal::new()),
                Rights::READ,
            ))
            .map(|_| ())
        })
        .expect("dummy fills second slot");
    rt.set_handle_table(table);

    // bootstrap_handle (x3) = 0 - невалидный стартовый хэндл.
    let err = dispatch(
        SyscallOp::ProcessStart,
        [
            u64::from(process_id.raw().get()),
            0x4000_0000,
            0x4001_0000,
            0,
            1,
            0,
        ],
    )
    .expect_err("zero bootstrap handle rejected");
    assert_eq!(err, SyscallError::InvalidArgument);
    assert_eq!(
        stub.start_user_process_calls.load(Ordering::SeqCst),
        0,
        "start_user_process must NOT be called for a zero handle",
    );
}

#[test]
fn process_start_succeeds_when_drain_frees_caller_slot() {
    // Пере-передача bootstrap-handle-а освобождает слот в caller-table под
    // возвращаемый thread-handle: syscall не должен отказывать пре-резервацией.
    let _guard = test_lock();
    let (rt, stub) = shared_runtime();
    stub.reset_counters();

    // capacity=2: нет свободных слотов; drain bootstrap-handle-а освободит ровно один.
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(2)));
    let process_id = install_process_handle(&table);
    let bootstrap_id = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Signal(Signal::new()),
                Rights::READ | Rights::TRANSFER,
            ))
        })
        .expect("insert bootstrap handle");
    rt.set_handle_table(table.clone());

    let ret = dispatch(
        SyscallOp::ProcessStart,
        [
            u64::from(process_id.raw().get()),
            0x4000_0000,
            0x4001_0000,
            u64::from(bootstrap_id.raw().get()),
            1,
            0,
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
