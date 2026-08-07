//! Интеграционные тесты port-IPC хендлеров (`syscall::port`).
//!
//! `Reply::new`/`port_call`-flow требуют двух потоков с реальными транспортами
//! (недоступно в host-тесте); здесь проверяем только то, что достижимо через
//! `KernelRuntime`: создание Port-хендла и корректный `IpcError`-маппинг.

#![allow(clippy::unwrap_used)]

use core::{num::NonZeroU64, sync::atomic::AtomicU32};
use std::sync::{Arc, Mutex, OnceLock};

use capability::{
    Capability, CapabilityTarget, HandleTable, KernelRuntime, Rights, Signal, WaitToken,
    install_runtime,
};
use collections::{LockCell, MutexCell};
use memory::{
    MemFlags, UserVmContext,
    memory_mapper::{
        AddressSpaceHandle, AddressSpaceTag, MemoryMapper, MemoryMappingError,
        MemoryRemappingError, MemoryUnmappingError, UserCopyError,
    },
    physical_address::{PageAlignedAddress, PhysicalAddress},
    user_vm_allocator::UserVmAllocator,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use syscall_kernel::SyscallError;

struct TableRuntime {
    handle_table: Mutex<Option<Arc<MutexCell<HandleTable>>>>,
}

impl TableRuntime {
    fn new() -> Self {
        Self {
            handle_table: Mutex::new(None),
        }
    }

    fn set_handle_table(&self, table: Arc<MutexCell<HandleTable>>) {
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

    fn block_current_until(&self, _ready_flag: &AtomicU32, _timeout_ns: Option<u64>) {}

    fn unblock(&self, _token: WaitToken) {}

    fn set_blocked_cancel(&self, _cancel: Arc<dyn capability::CancelTarget>) {}

    fn clear_blocked_cancel(&self) {}
}

struct InertMapper;

impl MemoryMapper for InertMapper {
    fn map(
        &self,
        _va: PageAlignedVirtualAddress,
        _pages: usize,
        _init: &[u8],
        _flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        Err(MemoryMappingError::AlreadyMapped)
    }
    fn map_exact(
        &self,
        _va: PageAlignedVirtualAddress,
        _pa: PageAlignedAddress,
        _size: usize,
        _flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        Err(MemoryMappingError::AlreadyMapped)
    }
    fn unmap(
        &self,
        _va: PageAlignedVirtualAddress,
        _size: usize,
    ) -> Result<(), MemoryUnmappingError> {
        Err(MemoryUnmappingError::NotMapped)
    }
    fn remap(
        &self,
        _va: PageAlignedVirtualAddress,
        _size: usize,
        _flags: MemFlags,
    ) -> Result<(), MemoryRemappingError> {
        Err(MemoryRemappingError::NotMapped)
    }
    fn activate_handle(&self) -> AddressSpaceHandle {
        AddressSpaceHandle::new(PhysicalAddress::new(0), AddressSpaceTag::NONE)
    }
    fn zero_owned_frame(&self, _pa: PageAlignedAddress) {}
    fn copy_user_in(&self, _va: VirtualAddress, _dst: &mut [u8]) -> Result<(), UserCopyError> {
        Err(UserCopyError::NotMapped)
    }
    fn copy_user_out(&self, _va: VirtualAddress, _src: &[u8]) -> Result<(), UserCopyError> {
        Err(UserCopyError::NotMapped)
    }
    fn as_any(&self) -> &(dyn core::any::Any + 'static) {
        self
    }
}

struct StubSyscallRuntime;

impl syscall_kernel::SyscallRuntime for StubSyscallRuntime {
    fn current_user_vm(&self) -> Option<UserVmContext> {
        let allocator = Arc::new(MutexCell::new(UserVmAllocator::new(
            PageAlignedVirtualAddress::from_usize(0x1000_0000)?,
            VirtualAddress::new(0x1100_0000),
        )));
        Some(UserVmContext::new(Arc::new(InertMapper), allocator))
    }

    fn current_ipc_buffer_va(&self) -> Option<u64> {
        Some(0x3000_0000)
    }

    fn frame_allocator(
        &self,
    ) -> Option<&'static (dyn memory::frame_allocator::FrameAllocator + Send + Sync)> {
        None
    }

    fn current_thread_object(&self) -> Option<Arc<capability::ThreadObject>> {
        None
    }

    fn current_process_object(&self) -> Option<Arc<capability::ProcessObject>> {
        None
    }

    fn create_empty_process(
        &self,
        _name: &str,
    ) -> Result<Arc<capability::ProcessObject>, capability::SpawnError> {
        Err(capability::SpawnError::NoFreeProcessSlots)
    }

    fn create_user_thread(
        &self,
        _process: &Arc<capability::ProcessObject>,
        _entry: capability::UserThreadEntry,
    ) -> Result<Arc<capability::ThreadObject>, capability::SpawnError> {
        Err(capability::SpawnError::NoFreeThreadSlots)
    }

    fn terminate_thread(
        &self,
        _thread: &Arc<capability::ThreadObject>,
        _exit_code: i32,
    ) -> Result<(), capability::IpcError> {
        Ok(())
    }

    fn terminate_process(
        &self,
        _process: &Arc<capability::ProcessObject>,
        _exit_code: i32,
    ) -> Result<(), capability::IpcError> {
        Ok(())
    }

    fn load_user_image_into(
        &self,
        _process: &Arc<capability::ProcessObject>,
        _install: &capability::UserImageInstall,
    ) -> Result<(), capability::LoadImageError> {
        Err(capability::LoadImageError::ProcessNotFound)
    }

    fn start_user_process(
        &self,
        _process: &Arc<capability::ProcessObject>,
        _spec: capability::UserStartSpec,
    ) -> Result<Arc<capability::ThreadObject>, capability::StartProcessError> {
        Err(capability::StartProcessError::ProcessNotFound)
    }
}

fn runtime() -> &'static Arc<TableRuntime> {
    static RUNTIME: OnceLock<Arc<TableRuntime>> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let rt = Arc::new(TableRuntime::new());
        let dyn_rt: Arc<dyn KernelRuntime> = rt.clone();
        install_runtime(dyn_rt);
        syscall_kernel::install_runtime(Arc::new(StubSyscallRuntime));
        rt
    })
}

fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

use syscall_kernel::{Origin, SyscallFrame, SyscallOp};

struct TestFrame {
    op: u16,
    args: [u64; 6],
    ret: i64,
}

impl TestFrame {
    fn new(op: SyscallOp, args: [u64; 6]) -> Self {
        Self {
            op: op as u16,
            args,
            ret: 0,
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
    fn set_secondary_return(&mut self, _value: u64) {}
    fn origin(&self) -> Origin {
        Origin::User
    }
}

fn dispatch(op: SyscallOp, args: [u64; 6]) -> Result<u64, i64> {
    let mut frame = TestFrame::new(op, args);
    syscall_kernel::dispatch(&mut frame);
    if frame.ret >= 0 {
        Ok(u64::try_from(frame.ret).unwrap())
    } else {
        Err(frame.ret)
    }
}

fn err_code(e: SyscallError) -> i64 {
    e.as_return_value()
}

#[test]
fn port_create_registers_usable_port_handle() {
    let _guard = test_lock();
    let rt = runtime();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    rt.set_handle_table(table.clone());

    let id = dispatch(SyscallOp::PortCreate, [0; 6]).expect("port create succeeds");
    assert!(id != 0, "fresh port handle id must be non-zero");

    let raw = u32::try_from(id).unwrap();
    let handle_id = capability::HandleId::from_raw(core::num::NonZeroU32::new(raw).unwrap());
    table.with_lock(|tbl| {
        tbl.get_port(handle_id, Rights::READ)
            .expect("port readable");
        tbl.get_port(handle_id, Rights::WRITE)
            .expect("port writable");
    });
}

#[test]
fn port_send_on_non_port_handle_is_wrong_type() {
    let _guard = test_lock();
    let rt = runtime();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    let sig_id = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Signal(Signal::new()),
                Rights::READ | Rights::WRITE,
            ))
        })
        .expect("insert signal");
    rt.set_handle_table(table);

    let raw = u64::from(sig_id.raw().get());
    // send/call/recv на Signal (не Port) -> WrongType.
    assert_eq!(
        dispatch(SyscallOp::PortSend, [raw, 0, 0, 0, 0, 0]),
        Err(err_code(SyscallError::WrongType))
    );
    assert_eq!(
        dispatch(SyscallOp::PortCall, [raw, 0, 0, 0, 0, 0]),
        Err(err_code(SyscallError::WrongType))
    );
    assert_eq!(
        dispatch(SyscallOp::PortRecv, [raw, 0, 0, 0, 0, 0]),
        Err(err_code(SyscallError::WrongType))
    );
}

#[test]
fn port_reply_on_non_reply_handle_is_wrong_type() {
    let _guard = test_lock();
    let rt = runtime();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    let sig_id = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Signal(Signal::new()),
                Rights::WRITE,
            ))
        })
        .expect("insert signal");
    rt.set_handle_table(table);

    assert_eq!(
        dispatch(
            SyscallOp::PortReply,
            [u64::from(sig_id.raw().get()), 0, 0, 0, 0, 0]
        ),
        Err(err_code(SyscallError::WrongType))
    );
}

#[test]
fn port_send_on_closed_handle_is_bad_handle() {
    let _guard = test_lock();
    let rt = runtime();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    rt.set_handle_table(table);

    assert_eq!(
        dispatch(SyscallOp::PortSend, [5, 0, 0, 0, 0, 0]),
        Err(err_code(SyscallError::BadHandle))
    );
}

#[test]
fn port_send_zero_handle_is_invalid_argument() {
    let _guard = test_lock();
    let rt = runtime();
    rt.set_handle_table(Arc::new(MutexCell::new(HandleTable::with_capacity(1))));
    assert_eq!(
        dispatch(SyscallOp::PortSend, [0, 0, 0, 0, 0, 0]),
        Err(err_code(SyscallError::InvalidArgument))
    );
}
