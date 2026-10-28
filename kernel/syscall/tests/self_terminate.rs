//! Операции syscall над хендлом собственного процесса и потока.
//!
//! Self-terminate через handle отвергается с `AccessDenied` в `sys_process_terminate` и
//! `sys_thread_terminate`: self-exit обязан идти через `ThreadExit` либо `ProcessExit`,
//! выполняющие context switch. Капа, которую выдаёт `ProcessSelf`, держателем процесса не
//! считается.

#![allow(clippy::unwrap_used)]
#![allow(clippy::indexing_slicing, clippy::panic, clippy::unwrap_in_result)]

mod common;

use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicU32, Ordering},
};
use std::sync::{Arc, Mutex, OnceLock};

use capability::{
    Capability, CapabilityTarget, HandleId, HandleTable, IpcError, KernelRuntime, LoadImageError,
    ProcessObject, Rights, SpawnError, StartProcessError, ThreadObject, UserImageInstall,
    UserStartSpec, UserThreadEntry, WaitToken, install_runtime,
};
use kbase::{LockCell, MutexCell};
use memory::{UserVmContext, frame_allocator::FrameAllocator};
use syscall_kernel::{SyscallError, SyscallOp, SyscallRuntime};

use crate::common::{dispatch, test_lock};

const ORPHANED_EXIT_CODE: i32 = -2;

struct SelfRuntime {
    handle_table: Mutex<Option<Arc<MutexCell<HandleTable>>>>,
    current_process: Mutex<Option<Arc<ProcessObject>>>,
    current_thread: Mutex<Option<Arc<ThreadObject>>>,
    thread_in_current_process: Mutex<Option<bool>>,
    terminated_threads: AtomicU32,
    orphaned: AtomicU32,
    exited_cpu: AtomicU32,
}

impl SelfRuntime {
    fn new() -> Self {
        Self {
            handle_table: Mutex::new(None),
            current_process: Mutex::new(None),
            current_thread: Mutex::new(None),
            thread_in_current_process: Mutex::new(None),
            terminated_threads: AtomicU32::new(0),
            orphaned: AtomicU32::new(0),
            exited_cpu: AtomicU32::new(0),
        }
    }

    fn take_orphaned(&self) -> u32 {
        self.orphaned.swap(0, Ordering::AcqRel)
    }

    fn take_exited_cpu(&self) -> u32 {
        self.exited_cpu.swap(0, Ordering::AcqRel)
    }

    fn take_terminated_threads(&self) -> u32 {
        self.terminated_threads.swap(0, Ordering::AcqRel)
    }
}

impl KernelRuntime for SelfRuntime {
    fn current_wait_token(&self) -> WaitToken {
        WaitToken::new(NonZeroU64::new(1).unwrap())
    }
    fn now_ns(&self) -> u64 {
        0
    }
    fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
        self.handle_table.lock().unwrap().clone()
    }
    fn exit_current_thread(&self, _c: i32) -> ! {
        panic!("must not exit");
    }
    fn exit_current_process(&self, _c: i32) -> ! {
        panic!("must not exit process");
    }
    fn yield_current(&self) {}
    fn block_current_until(&self, _r: &AtomicU32, _t: Option<u64>) {}
    fn unblock(&self, _t: WaitToken) {}
    fn set_blocked_cancel(&self, _cancel: Arc<dyn capability::CancelTarget>) {}
    fn clear_blocked_cancel(&self) {}
    fn release_process_holder(&self, process: &Arc<capability::ProcessObject>) {
        process.signal_terminated(ORPHANED_EXIT_CODE);
        self.orphaned.fetch_add(1, Ordering::AcqRel);
    }

    fn exit_if_requested(&self) {
        self.exited_cpu.fetch_add(1, Ordering::AcqRel);
    }
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
    fn thread_in_current_process(&self, _t: &Arc<ThreadObject>) -> Option<bool> {
        *self.thread_in_current_process.lock().unwrap()
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
        self.terminated_threads.fetch_add(1, Ordering::AcqRel);
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

fn terminate(op: SyscallOp, handle: HandleId) -> Result<u64, i64> {
    dispatch(op, [u64::from(handle.raw().get()), 0, 0, 0, 0, 0])
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
#[should_panic(expected = "must not exit process")]
fn process_exit_reaches_the_runtime() {
    let _g = test_lock();
    let _rt = runtime();

    let _ = dispatch(SyscallOp::ProcessExit, [7, 0, 0, 0, 0, 0]);
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

fn install_thread_handle(rt: &SelfRuntime, in_current_process: Option<bool>) -> HandleId {
    *rt.current_thread.lock().unwrap() = Some(ThreadObject::new());
    *rt.current_process.lock().unwrap() = None;
    *rt.thread_in_current_process.lock().unwrap() = in_current_process;
    rt.take_terminated_threads();

    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    let tid = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Thread(ThreadObject::new()),
                Rights::WRITE,
            ))
        })
        .unwrap();
    *rt.handle_table.lock().unwrap() = Some(table);
    tid
}

#[test]
fn thread_terminate_on_thread_of_other_process_is_access_denied() {
    let _g = test_lock();
    let rt = runtime();
    let tid = install_thread_handle(rt, Some(false));

    assert_eq!(
        terminate(SyscallOp::ThreadTerminate, tid),
        Err(SyscallError::AccessDenied.as_return_value())
    );
    assert_eq!(rt.take_terminated_threads(), 0);
}

#[test]
fn thread_terminate_on_thread_of_own_process_succeeds() {
    let _g = test_lock();
    let rt = runtime();
    let tid = install_thread_handle(rt, Some(true));

    assert_eq!(terminate(SyscallOp::ThreadTerminate, tid), Ok(0));
    assert_eq!(rt.take_terminated_threads(), 1);
}

#[test]
fn process_terminate_on_other_handle_succeeds() {
    // Терминирование другого процесса (не current) проходит.
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

fn close(handle: HandleId) -> Result<u64, i64> {
    dispatch(
        SyscallOp::HandleClose,
        [u64::from(handle.raw().get()), 0, 0, 0, 0, 0],
    )
}

fn process_self() -> Result<HandleId, i64> {
    let raw = dispatch(SyscallOp::ProcessSelf, [0; 6])?;
    let raw = u32::try_from(raw).unwrap();
    Ok(HandleId::from_raw(core::num::NonZeroU32::new(raw).unwrap()))
}

#[test]
fn closing_the_self_capability_leaves_the_process_running() {
    let _g = test_lock();
    let rt = runtime();
    let process = ProcessObject::new();
    *rt.current_process.lock().unwrap() = Some(process.clone());
    *rt.current_thread.lock().unwrap() = None;

    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    *rt.handle_table.lock().unwrap() = Some(table.clone());
    let owner = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Process(process.clone()),
                Rights::READ | Rights::WRITE,
            ))
        })
        .expect("insert owner handle");
    rt.take_orphaned();

    let self_handle = process_self().expect("process_self");
    assert_eq!(close(self_handle), Ok(0));
    assert_eq!(rt.take_orphaned(), 0);
    assert!(!process.terminated());

    assert_eq!(close(owner), Ok(0));
    assert_eq!(rt.take_orphaned(), 1);
    assert!(process.terminated());
}

#[test]
fn closing_the_last_holder_of_the_current_process_returns_from_the_syscall() {
    let _g = test_lock();
    let rt = runtime();
    let process = ProcessObject::new();
    *rt.current_process.lock().unwrap() = Some(process.clone());
    *rt.current_thread.lock().unwrap() = None;

    // Единственный учитываемый держатель лежит в таблице самого процесса. Такую капу
    // кладёт туда перенос по IPC либо стартовый хэндл `ProcessStart`.
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    *rt.handle_table.lock().unwrap() = Some(table.clone());
    let owner = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Process(process.clone()),
                Rights::READ | Rights::WRITE,
            ))
        })
        .expect("insert owner handle");
    rt.take_orphaned();
    rt.take_exited_cpu();

    assert_eq!(close(owner), Ok(0));

    assert_eq!(rt.take_orphaned(), 1);
    assert_eq!(
        rt.take_exited_cpu(),
        0,
        "the dispatcher returns, and the thread exits at the EL0 boundary"
    );
}

#[test]
fn closing_the_last_holder_reports_the_removal_to_the_observer() {
    let _g = test_lock();
    let rt = runtime();
    let process = ProcessObject::new();
    *rt.current_process.lock().unwrap() = Some(ProcessObject::new());
    *rt.current_thread.lock().unwrap() = None;

    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    *rt.handle_table.lock().unwrap() = Some(table.clone());
    let owner = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Process(process.clone()),
                Rights::READ | Rights::WRITE,
            ))
        })
        .expect("insert owner handle");
    let observer = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Process(process.clone()),
                Rights::READ,
            ))
        })
        .expect("insert observer handle");
    let signal = process.termination_signal();
    rt.take_orphaned();

    assert_eq!(close(owner), Ok(0));

    assert_eq!(signal.peek() & capability::SIGNALED, capability::SIGNALED);
    assert_eq!(process.exit_code(), ORPHANED_EXIT_CODE);
    assert_eq!(close(observer), Ok(0));
}
