#![allow(dead_code)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::indexing_slicing, clippy::panic)]

use core::{num::NonZeroU64, sync::atomic::AtomicU32};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use capability::{
    CancelTarget, HandleTable, IpcError, KernelRuntime, LoadImageError, ProcessObject, SpawnError,
    StartProcessError, ThreadObject, UserImageInstall, UserStartSpec, UserThreadEntry, WaitToken,
    install_runtime,
};
use kbase::MutexCell;
use memory::{
    UserVmContext,
    frame_allocator::FrameAllocator,
    memory_mapper::MemoryMapper,
    user_vm_allocator::UserVmAllocator,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use syscall_kernel::{Origin, SyscallFrame, SyscallOp, SyscallRuntime};

pub(crate) const USER_VM_ARENA_BASE: usize = 0x1000_0000;
pub(crate) const USER_VM_ARENA_END: usize = 0x1100_0000;

type UserVm = (
    Arc<dyn MemoryMapper + Send + Sync>,
    Arc<MutexCell<UserVmAllocator>>,
);

pub(crate) struct TableRuntime {
    handle_table: Mutex<Option<Arc<MutexCell<HandleTable>>>>,
}

#[derive(Default)]
pub(crate) struct StubSyscallRuntime {
    user_vm: Mutex<Option<UserVm>>,
    ipc_buffer_va: Mutex<Option<u64>>,
    frame_allocator: Mutex<Option<&'static (dyn FrameAllocator + Send + Sync)>>,
}

pub(crate) struct TestFrame {
    op: u16,
    args: [u64; 6],
    ret: i64,
}

impl TableRuntime {
    fn new() -> Self {
        Self {
            handle_table: Mutex::new(None),
        }
    }

    pub(crate) fn set_handle_table(&self, table: Arc<MutexCell<HandleTable>>) {
        *self.handle_table.lock().unwrap() = Some(table);
    }
}

impl KernelRuntime for TableRuntime {
    fn current_wait_token(&self) -> WaitToken {
        WaitToken::new(NonZeroU64::new(1).unwrap())
    }
    fn now_ns(&self) -> u64 {
        0
    }
    fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
        self.handle_table.lock().unwrap().clone()
    }
    fn exit_current_thread(&self, _exit_code: i32) -> ! {
        panic!("exit_current_thread must not be called");
    }
    fn exit_current_process(&self, _exit_code: i32) -> ! {
        panic!("exit_current_process must not be called");
    }
    fn yield_current(&self) {}
    fn block_current_until(&self, _ready_flag: &AtomicU32, _timeout_ns: Option<u64>) {}
    fn unblock(&self, _token: WaitToken) {}
    fn set_blocked_cancel(&self, _cancel: Arc<dyn CancelTarget>) {}
    fn clear_blocked_cancel(&self) {}
    fn release_process_holder(&self, _process: &Arc<ProcessObject>) {}
}

impl StubSyscallRuntime {
    pub(crate) fn set_mapper(&self, mapper: Arc<dyn MemoryMapper + Send + Sync>) {
        self.set_user_vm(mapper, user_vm_allocator());
    }

    pub(crate) fn set_user_vm(
        &self,
        mapper: Arc<dyn MemoryMapper + Send + Sync>,
        allocator: Arc<MutexCell<UserVmAllocator>>,
    ) {
        *self.user_vm.lock().unwrap() = Some((mapper, allocator));
    }

    pub(crate) fn clear_user_vm(&self) {
        *self.user_vm.lock().unwrap() = None;
    }

    pub(crate) fn set_ipc_buffer_va(&self, va: u64) {
        *self.ipc_buffer_va.lock().unwrap() = Some(va);
    }

    pub(crate) fn set_frame_allocator(
        &self,
        allocator: &'static (dyn FrameAllocator + Send + Sync),
    ) {
        *self.frame_allocator.lock().unwrap() = Some(allocator);
    }
}

impl SyscallRuntime for StubSyscallRuntime {
    fn current_user_vm(&self) -> Option<UserVmContext> {
        let (mapper, allocator) = self.user_vm.lock().unwrap().clone()?;
        Some(UserVmContext::new(mapper, allocator))
    }
    fn current_ipc_buffer_va(&self) -> Option<u64> {
        *self.ipc_buffer_va.lock().unwrap()
    }
    fn frame_allocator(&self) -> Option<&'static (dyn FrameAllocator + Send + Sync)> {
        *self.frame_allocator.lock().unwrap()
    }
    fn current_thread_object(&self) -> Option<Arc<ThreadObject>> {
        None
    }
    fn current_process_object(&self) -> Option<Arc<ProcessObject>> {
        None
    }
    fn thread_in_current_process(&self, _thread: &Arc<ThreadObject>) -> Option<bool> {
        None
    }
    fn create_empty_process(&self, _name: &str) -> Result<Arc<ProcessObject>, SpawnError> {
        Ok(ProcessObject::new())
    }
    fn create_user_thread(
        &self,
        _process: &Arc<ProcessObject>,
        _entry: UserThreadEntry,
    ) -> Result<Arc<ThreadObject>, SpawnError> {
        Ok(ThreadObject::new())
    }
    fn terminate_thread(&self, _thread: &Arc<ThreadObject>, _code: i32) -> Result<(), IpcError> {
        Ok(())
    }
    fn terminate_process(&self, _process: &Arc<ProcessObject>, _code: i32) -> Result<(), IpcError> {
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
        _spec: UserStartSpec,
    ) -> Result<Arc<ThreadObject>, StartProcessError> {
        Ok(ThreadObject::new())
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
    fn set_secondary_return(&mut self, _value: u64) {}
    fn origin(&self) -> Origin {
        Origin::User
    }
}

pub(crate) fn install(syscall: Arc<dyn SyscallRuntime>) -> Arc<TableRuntime> {
    let kernel = Arc::new(TableRuntime::new());
    let dyn_kernel: Arc<dyn KernelRuntime> = kernel.clone();
    install_runtime(dyn_kernel);
    syscall_kernel::install_runtime(syscall);
    kernel
}

pub(crate) fn runtimes() -> &'static (Arc<TableRuntime>, Arc<StubSyscallRuntime>) {
    static RUNTIMES: OnceLock<(Arc<TableRuntime>, Arc<StubSyscallRuntime>)> = OnceLock::new();
    RUNTIMES.get_or_init(|| {
        let stub = Arc::new(StubSyscallRuntime::default());
        (install(stub.clone()), stub)
    })
}

pub(crate) fn user_vm_allocator() -> Arc<MutexCell<UserVmAllocator>> {
    Arc::new(MutexCell::new(UserVmAllocator::new(
        PageAlignedVirtualAddress::from_usize(USER_VM_ARENA_BASE).unwrap(),
        VirtualAddress::new(USER_VM_ARENA_END),
    )))
}

pub(crate) fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn dispatch(op: SyscallOp, args: [u64; 6]) -> Result<u64, i64> {
    let mut frame = TestFrame {
        op: op as u16,
        args,
        ret: 0,
    };
    syscall_kernel::dispatch(&mut frame);
    u64::try_from(frame.ret).map_err(|_| frame.ret)
}
