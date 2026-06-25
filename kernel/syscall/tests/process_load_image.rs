//! Валидация spawn-ABI в `sys_process_load_image` (`SyscallOp::ProcessLoadImage`):
//! неверная версия, `segment_count` вне границ, `reserved != 0`,
//! несовпадение `mapped_size`, `AccessDenied` при слабой access-mask.

use core::{num::NonZeroUsize, sync::atomic::AtomicU32};
use std::sync::{Arc, Mutex, OnceLock};

use capability::{
    Capability, CapabilityTarget, HandleId, HandleTable, IpcError, KernelRuntime, LoadImageError,
    ProcessObject, Rights, SpawnError, StartProcessError, ThreadObject, UserImageInstall,
    UserStartSpec, UserThreadEntry, WaitToken, default_rights_for, install_runtime,
};
use collections::{LockCell, MutexCell};
use memory::{
    AccessMask, MemFlags, MemoryRegion, UserVmContext,
    frame_allocator::FrameAllocator,
    memory_mapper::{
        AddressSpaceHandle, AddressSpaceTag, MemoryMapper, MemoryMappingError,
        MemoryRemappingError, MemoryUnmappingError, UserCopyError,
    },
    physical_address::{PageAlignedAddress, PhysicalAddress},
    user_vm_allocator::UserVmAllocator,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use syscall_kernel::{
    Origin, SyscallError, SyscallFrame, SyscallOp, USER_IMAGE_DESC_SIZE, USER_SEGMENT_SIZE,
};

const BASE_VA: usize = 0x2000_0000;
const SEG_VA_BASE: usize = 0x4000_0000;
const REGION_SIZE: usize = 0x1000;

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
        WaitToken::new(core::num::NonZeroU64::new(1).unwrap())
    }
    fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
        self.handle_table.lock().unwrap().clone()
    }
    fn exit_current_thread(&self, _exit_code: i32) -> ! {
        panic!("must not exit");
    }
    fn block_current_until(&self, _ready_flag: &AtomicU32, _timeout_ns: Option<u64>) {}
    fn unblock(&self, _token: WaitToken) {}
    fn set_blocked_cancel(&self, _cancel: Arc<dyn capability::CancelTarget>) {}
    fn clear_blocked_cancel(&self) {}
}

struct CannedMapper {
    bytes: Mutex<std::vec::Vec<u8>>,
}

impl MemoryMapper for CannedMapper {
    fn map(
        &self,
        _va: PageAlignedVirtualAddress,
        _pc: usize,
        _init: &[u8],
        _f: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        unimplemented!()
    }
    fn map_exact(
        &self,
        _va: PageAlignedVirtualAddress,
        _pa: PageAlignedAddress,
        _s: usize,
        _f: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        unimplemented!()
    }
    fn unmap(&self, _va: PageAlignedVirtualAddress, _s: usize) -> Result<(), MemoryUnmappingError> {
        unimplemented!()
    }
    fn remap(
        &self,
        _va: PageAlignedVirtualAddress,
        _s: usize,
        _f: MemFlags,
    ) -> Result<(), MemoryRemappingError> {
        unimplemented!()
    }
    fn activate_handle(&self) -> AddressSpaceHandle {
        AddressSpaceHandle::new(PhysicalAddress::new(0), AddressSpaceTag::NONE)
    }
    fn zero_owned_frame(&self, _pa: PageAlignedAddress) {
        unimplemented!()
    }
    fn copy_user_in(&self, va: VirtualAddress, dst: &mut [u8]) -> Result<(), UserCopyError> {
        let bytes = self.bytes.lock().unwrap();
        let offset = va.as_usize().wrapping_sub(BASE_VA);
        let end = offset
            .checked_add(dst.len())
            .ok_or(UserCopyError::NotMapped)?;
        if va.as_usize() < BASE_VA || end > bytes.len() {
            return Err(UserCopyError::NotMapped);
        }
        dst.copy_from_slice(&bytes[offset..end]);
        Ok(())
    }
    fn as_any(&self) -> &(dyn core::any::Any + 'static) {
        self
    }
}

struct StubSyscallRuntime {
    user_vm: Mutex<Option<Arc<CannedMapper>>>,
}

impl StubSyscallRuntime {
    fn new() -> Self {
        Self {
            user_vm: Mutex::new(None),
        }
    }
    fn set_bytes(&self, bytes: std::vec::Vec<u8>) {
        *self.user_vm.lock().unwrap() = Some(Arc::new(CannedMapper {
            bytes: Mutex::new(bytes),
        }));
    }
}

impl syscall_kernel::SyscallRuntime for StubSyscallRuntime {
    fn current_user_vm(&self) -> Option<UserVmContext> {
        let mapper = self.user_vm.lock().unwrap().clone()?;
        let allocator = Arc::new(MutexCell::new(UserVmAllocator::new(
            PageAlignedVirtualAddress::from_usize(0x1000_0000).unwrap(),
            VirtualAddress::new(0x1100_0000),
        )));
        Some(UserVmContext::new(mapper, allocator))
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
    fn create_empty_process(&self, _name: &str) -> Result<Arc<ProcessObject>, SpawnError> {
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

fn runtimes() -> &'static (Arc<TableRuntime>, Arc<StubSyscallRuntime>) {
    static R: OnceLock<(Arc<TableRuntime>, Arc<StubSyscallRuntime>)> = OnceLock::new();
    R.get_or_init(|| {
        let rt = Arc::new(TableRuntime::new());
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

fn encode_desc(version: u32, segment_count: u32, segments_va: u64) -> [u8; USER_IMAGE_DESC_SIZE] {
    let mut d = [0u8; USER_IMAGE_DESC_SIZE];
    d[0..4].copy_from_slice(&version.to_le_bytes());
    d[4..8].copy_from_slice(&segment_count.to_le_bytes());
    d[8..16].copy_from_slice(&segments_va.to_le_bytes());
    d[16..24].copy_from_slice(&(SEG_VA_BASE as u64).to_le_bytes());
    d[24..32].copy_from_slice(&0x5000_0000u64.to_le_bytes());
    d[32..40].copy_from_slice(&0x4000u64.to_le_bytes());
    d[40..48].copy_from_slice(&0x6000_0000u64.to_le_bytes());
    d[48..56].copy_from_slice(&0x1_0000u64.to_le_bytes());
    d
}

fn encode_segment(
    region_handle: u32,
    flags: u32,
    mapped_size: u64,
    reserved: u64,
) -> [u8; USER_SEGMENT_SIZE] {
    let mut s = [0u8; USER_SEGMENT_SIZE];
    s[0..4].copy_from_slice(&region_handle.to_le_bytes());
    s[4..8].copy_from_slice(&flags.to_le_bytes());
    s[8..16].copy_from_slice(&(SEG_VA_BASE as u64).to_le_bytes());
    s[16..24].copy_from_slice(&mapped_size.to_le_bytes());
    s[24..32].copy_from_slice(&reserved.to_le_bytes());
    s
}

fn canned_buffer(
    desc: [u8; USER_IMAGE_DESC_SIZE],
    segment: Option<[u8; USER_SEGMENT_SIZE]>,
) -> std::vec::Vec<u8> {
    let mut v = desc.to_vec();
    if let Some(seg) = segment {
        v.extend_from_slice(&seg);
    }
    v
}

fn insert_process(table: &Arc<MutexCell<HandleTable>>) -> HandleId {
    table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Process(ProcessObject::new()),
                Rights::WRITE,
            ))
        })
        .expect("insert process")
}

fn insert_region(table: &Arc<MutexCell<HandleTable>>, size: usize, access: AccessMask) -> HandleId {
    insert_region_with_rights(table, size, access, None)
}

/// Как [`insert_region`], но позволяет задать права хендла независимо от
/// access-mask региона - нужно, чтобы изолировать проверку маски (строка
/// `process.rs:184`) от проверки прав хендла (`:178`).
fn insert_region_with_rights(
    table: &Arc<MutexCell<HandleTable>>,
    size: usize,
    access: AccessMask,
    rights: Option<Rights>,
) -> HandleId {
    let region = Arc::new(MemoryRegion::create_physical(
        PageAlignedAddress::from_usize(0x8000_0000).unwrap(),
        NonZeroUsize::new(size).unwrap(),
        access,
    ));
    let target = CapabilityTarget::Memory(region);
    let rights = rights.unwrap_or_else(|| default_rights_for(&target));
    table
        .with_lock(|tbl| tbl.insert(Capability::new(target, rights)))
        .expect("insert region")
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

fn load_image(process_id: HandleId) -> Result<u64, i64> {
    let mut frame = TestFrame {
        op: SyscallOp::ProcessLoadImage as u16,
        args: [
            u64::from(process_id.raw().get()),
            BASE_VA as u64,
            USER_IMAGE_DESC_SIZE as u64,
            0,
            0,
            0,
        ],
        ret: 0,
    };
    syscall_kernel::dispatch(&mut frame);
    if frame.ret >= 0 {
        Ok(u64::try_from(frame.ret).unwrap())
    } else {
        Err(frame.ret)
    }
}

fn expect(e: SyscallError) -> Result<u64, i64> {
    Err(e.as_return_value())
}

#[test]
fn rejects_wrong_abi_version() {
    let _g = test_lock();
    let (rt, stub) = runtimes();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    let pid = insert_process(&table);
    rt.set_handle_table(table);
    stub.set_bytes(canned_buffer(
        encode_desc(2, 1, (BASE_VA + 56) as u64),
        None,
    ));
    assert_eq!(load_image(pid), expect(SyscallError::InvalidArgument));
}

#[test]
fn rejects_zero_segment_count() {
    let _g = test_lock();
    let (rt, stub) = runtimes();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    let pid = insert_process(&table);
    rt.set_handle_table(table);
    stub.set_bytes(canned_buffer(
        encode_desc(1, 0, (BASE_VA + 56) as u64),
        None,
    ));
    assert_eq!(load_image(pid), expect(SyscallError::InvalidArgument));
}

#[test]
fn rejects_segment_count_above_max() {
    let _g = test_lock();
    let (rt, stub) = runtimes();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    let pid = insert_process(&table);
    rt.set_handle_table(table);
    let too_many = (syscall_kernel::MAX_SEGMENTS_PER_IMG as u32) + 1;
    stub.set_bytes(canned_buffer(
        encode_desc(1, too_many, (BASE_VA + 56) as u64),
        None,
    ));
    assert_eq!(load_image(pid), expect(SyscallError::InvalidArgument));
}

#[test]
fn rejects_nonzero_reserved_field() {
    let _g = test_lock();
    let (rt, stub) = runtimes();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    let pid = insert_process(&table);
    let region = insert_region(&table, REGION_SIZE, AccessMask::RW);
    rt.set_handle_table(table);
    let seg = encode_segment(region.raw().get(), 0, REGION_SIZE as u64, 1);
    stub.set_bytes(canned_buffer(
        encode_desc(1, 1, (BASE_VA + 56) as u64),
        Some(seg),
    ));
    assert_eq!(load_image(pid), expect(SyscallError::InvalidArgument));
}

#[test]
fn rejects_mapped_size_mismatch() {
    let _g = test_lock();
    let (rt, stub) = runtimes();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    let pid = insert_process(&table);
    let region = insert_region(&table, REGION_SIZE, AccessMask::RW);
    rt.set_handle_table(table);
    let seg = encode_segment(region.raw().get(), 0, (REGION_SIZE * 2) as u64, 0);
    stub.set_bytes(canned_buffer(
        encode_desc(1, 1, (BASE_VA + 56) as u64),
        Some(seg),
    ));
    assert_eq!(load_image(pid), expect(SyscallError::InvalidArgument));
}

#[test]
fn rejects_access_denied_when_region_mask_too_weak() {
    let _g = test_lock();
    let (rt, stub) = runtimes();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    let pid = insert_process(&table);
    // Хендл несёт READ|WRITE|EXECUTE (проходит проверку прав на строке :178),
    // но access-mask региона - только RW, поэтому проверка маски на :184
    // отвергает запрос сегмента на RX (flags=2) именно как AccessDenied.
    let region = insert_region_with_rights(
        &table,
        REGION_SIZE,
        AccessMask::RW,
        Some(Rights::READ | Rights::WRITE | Rights::EXECUTE),
    );
    rt.set_handle_table(table);
    let seg = encode_segment(region.raw().get(), 2, REGION_SIZE as u64, 0);
    stub.set_bytes(canned_buffer(
        encode_desc(1, 1, (BASE_VA + 56) as u64),
        Some(seg),
    ));
    assert_eq!(load_image(pid), expect(SyscallError::AccessDenied));
}
