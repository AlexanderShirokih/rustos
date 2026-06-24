//! Self-terminate через handle отвергается с `AccessDenied`
//! (`process.rs:116`, `thread.rs:107`): self-exit обязан идти через
//! `ThreadExit`, выполняющий context switch.

use core::{num::NonZeroU64, sync::atomic::AtomicU32};
use std::sync::{Arc, Mutex, OnceLock};

use capability::{
    Capability, CapabilityTarget, HandleId, HandleTable, IpcError, KernelRuntime, LoadImageError,
    ProcessObject, Rights, SpawnError, StartProcessError, ThreadObject, UserImageInstall,
    UserStartSpec, UserThreadEntry, WaitToken, install_runtime,
};
use collections::{LockCell, MutexCell};
use memory::{UserVmContext, frame_allocator::FrameAllocator};
use syscall_kernel::{Origin, SyscallError, SyscallFrame, SyscallOp, SyscallRuntime};

struct SelfRuntime {
    handle_table: Mutex<Option<Arc<MutexCell<HandleTable>>>>,
    current_process: Mutex<Option<Arc<ProcessObject>>>,
    current_thread: Mutex<Option<Arc<ThreadObject>>>,
}

impl SelfRuntime {
    fn new() -> Self {
        Self {
            handle_table: Mutex::new(None),
            current_process: Mutex::new(None),
            current_thread: Mutex::new(None),
        }
    }
}

impl KernelRuntime for SelfRuntime {
    fn current_wait_token(&self) -> WaitToken {
        WaitToken::new(NonZeroU64::new(1).unwrap())
    }
    fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
        self.handle_table.lock().unwrap().clone()
    }
    fn exit_current_thread(&self, _c: i32) -> ! {
        panic!("must not exit");
    }
    fn block_current_until(&self, _r: &AtomicU32, _t: Option<u64>) {}
    fn unblock(&self, _t: WaitToken) {}
    fn set_blocked_cancel(&self, _cancel: Arc<dyn capability::CancelTarget>) {}
    fn clear_blocked_cancel(&self) {}
}

impl SyscallRuntime for SelfRuntime {
    fn current_user_vm(&self) -> Option<UserVmContext> {
        None
    }
    fn current_ipc_buffer_va(&self) -> Option<u64> {
        None
    }
    fn frame_allocator(&self) -> Option<&'static (dyn FrameAllocator + Send + Sync)> {
        None
    }
    fn current_thread_object(&self) -> Option<Arc<ThreadObject>> {
        self.current_thread.lock().unwrap().clone()
    }
    fn current_process_object(&self) -> Option<Arc<ProcessObject>> {
        self.current_process.lock().unwrap().clone()
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

fn runtime() -> &'static Arc<SelfRuntime> {
    static R: OnceLock<Arc<SelfRuntime>> = OnceLock::new();
    R.get_or_init(|| {
        let rt = Arc::new(SelfRuntime::new());
        let dyn_rt: Arc<dyn KernelRuntime> = rt.clone();
        install_runtime(dyn_rt);
        syscall_kernel::install_runtime(rt.clone());
        rt
    })
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

fn terminate(op: SyscallOp, handle: HandleId) -> Result<u64, i64> {
    let mut frame = TestFrame {
        op: op as u16,
        args: [u64::from(handle.raw().get()), 0, 0, 0, 0, 0],
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
fn process_terminate_on_self_handle_is_access_denied() {
    let _g = test_lock();
    let rt = runtime();
    let process = ProcessObject::new();
    *rt.current_process.lock().unwrap() = Some(process.clone());
    *rt.current_thread.lock().unwrap() = None;

    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    let pid = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Process(process),
                Rights::WRITE,
            ))
        })
        .expect("insert self process handle");
    *rt.handle_table.lock().unwrap() = Some(table);

    assert_eq!(
        terminate(SyscallOp::ProcessTerminate, pid),
        Err(SyscallError::AccessDenied.as_return_value())
    );
}

#[test]
fn thread_terminate_on_self_handle_is_access_denied() {
    let _g = test_lock();
    let rt = runtime();
    let thread = ThreadObject::new();
    *rt.current_thread.lock().unwrap() = Some(thread.clone());
    *rt.current_process.lock().unwrap() = None;

    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    let tid = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Thread(thread),
                Rights::WRITE,
            ))
        })
        .expect("insert self thread handle");
    *rt.handle_table.lock().unwrap() = Some(table);

    assert_eq!(
        terminate(SyscallOp::ThreadTerminate, tid),
        Err(SyscallError::AccessDenied.as_return_value())
    );
}

#[test]
fn process_terminate_on_other_handle_succeeds() {
    // Контроль: терминирование ДРУГОГО процесса (не current) проходит.
    let _g = test_lock();
    let rt = runtime();
    *rt.current_process.lock().unwrap() = Some(ProcessObject::new());
    *rt.current_thread.lock().unwrap() = None;

    let other = ProcessObject::new();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    let pid = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Process(other),
                Rights::WRITE,
            ))
        })
        .expect("insert other process handle");
    *rt.handle_table.lock().unwrap() = Some(table);

    assert_eq!(terminate(SyscallOp::ProcessTerminate, pid), Ok(0));
}
