//! Валидация spawn-ABI в `sys_process_load_image` (`SyscallOp::ProcessLoadImage`):
//! неверная версия, `segment_count` вне границ, `reserved != 0`,
//! несовпадение `mapped_size`, `AccessDenied` при слабой access-mask.

#![allow(clippy::unwrap_used)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_in_result
)]

mod common;

use core::num::NonZeroUsize;
use std::sync::Arc;

use capability::{
    Capability, CapabilityTarget, HandleId, HandleTable, ProcessObject, Rights, default_rights_for,
};
use kbase::{LockCell, MutexCell};
use memory::{
    AccessMask, MemFlags, MemoryRegion, physical_address::PageAlignedAddress, testing::PageMapper,
};
use syscall_kernel::{SyscallError, SyscallOp, USER_IMAGE_DESC_SIZE, USER_SEGMENT_SIZE};

use crate::common::{dispatch, runtimes, test_lock};

const BASE_VA: usize = 0x2000_0000;
const SEG_VA_BASE: usize = 0x4000_0000;
const REGION_SIZE: usize = 0x1000;

fn set_bytes(bytes: &[u8]) {
    runtimes()
        .1
        .set_mapper(PageMapper::with_mapping(BASE_VA, 1, bytes, MemFlags::user_ro()).unwrap());
}

fn encode_desc(version: u32, segment_count: u32, segments_va: u64) -> [u8; USER_IMAGE_DESC_SIZE] {
    let mut d = [0u8; USER_IMAGE_DESC_SIZE];
    d[0..4].copy_from_slice(&version.to_le_bytes());
    d[4..8].copy_from_slice(&segment_count.to_le_bytes());
    d[8..16].copy_from_slice(&segments_va.to_le_bytes());
    d[16..24].copy_from_slice(&(SEG_VA_BASE as u64).to_le_bytes());
    d[24..32].copy_from_slice(&0x6000_0000u64.to_le_bytes());
    d[32..40].copy_from_slice(&0x1_0000u64.to_le_bytes());
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

fn insert_region_with_rights(
    table: &Arc<MutexCell<HandleTable>>,
    size: usize,
    access: AccessMask,
    rights: Option<Rights>,
) -> HandleId {
    let region = Arc::new(MemoryRegion::create_physical_device(
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

fn load_image(process_id: HandleId) -> Result<u64, i64> {
    dispatch(
        SyscallOp::ProcessLoadImage,
        [
            u64::from(process_id.raw().get()),
            BASE_VA as u64,
            USER_IMAGE_DESC_SIZE as u64,
            0,
            0,
            0,
        ],
    )
}

fn expect(e: SyscallError) -> Result<u64, i64> {
    Err(e.as_return_value())
}

#[test]
fn rejects_wrong_abi_version() {
    let _g = test_lock();
    let (rt, _) = runtimes();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    let pid = insert_process(&table);
    rt.set_handle_table(table);
    set_bytes(&canned_buffer(
        encode_desc(2, 1, (BASE_VA + USER_IMAGE_DESC_SIZE) as u64),
        None,
    ));
    assert_eq!(load_image(pid), expect(SyscallError::InvalidArgument));
}

#[test]
fn rejects_zero_segment_count() {
    let _g = test_lock();
    let (rt, _) = runtimes();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    let pid = insert_process(&table);
    rt.set_handle_table(table);
    set_bytes(&canned_buffer(
        encode_desc(1, 0, (BASE_VA + USER_IMAGE_DESC_SIZE) as u64),
        None,
    ));
    assert_eq!(load_image(pid), expect(SyscallError::InvalidArgument));
}

#[test]
fn rejects_segment_count_above_max() {
    let _g = test_lock();
    let (rt, _) = runtimes();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    let pid = insert_process(&table);
    rt.set_handle_table(table);
    let too_many = (syscall_kernel::MAX_SEGMENTS_PER_IMG as u32) + 1;
    set_bytes(&canned_buffer(
        encode_desc(1, too_many, (BASE_VA + USER_IMAGE_DESC_SIZE) as u64),
        None,
    ));
    assert_eq!(load_image(pid), expect(SyscallError::InvalidArgument));
}

#[test]
fn rejects_nonzero_reserved_field() {
    let _g = test_lock();
    let (rt, _) = runtimes();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    let pid = insert_process(&table);
    let region = insert_region(&table, REGION_SIZE, AccessMask::RW);
    rt.set_handle_table(table);
    let seg = encode_segment(region.raw().get(), 0, REGION_SIZE as u64, 1);
    set_bytes(&canned_buffer(
        encode_desc(1, 1, (BASE_VA + USER_IMAGE_DESC_SIZE) as u64),
        Some(seg),
    ));
    assert_eq!(load_image(pid), expect(SyscallError::InvalidArgument));
}

#[test]
fn rejects_mapped_size_mismatch() {
    let _g = test_lock();
    let (rt, _) = runtimes();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    let pid = insert_process(&table);
    let region = insert_region(&table, REGION_SIZE, AccessMask::RW);
    rt.set_handle_table(table);
    let seg = encode_segment(region.raw().get(), 0, (REGION_SIZE * 2) as u64, 0);
    set_bytes(&canned_buffer(
        encode_desc(1, 1, (BASE_VA + USER_IMAGE_DESC_SIZE) as u64),
        Some(seg),
    ));
    assert_eq!(load_image(pid), expect(SyscallError::InvalidArgument));
}

#[test]
fn rejects_access_denied_when_region_mask_too_weak() {
    let _g = test_lock();
    let (rt, _) = runtimes();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(8)));
    let pid = insert_process(&table);
    // Хендл содержит READ|WRITE|EXECUTE (проходит проверку прав на строке :178),
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
    set_bytes(&canned_buffer(
        encode_desc(1, 1, (BASE_VA + USER_IMAGE_DESC_SIZE) as u64),
        Some(seg),
    ));
    assert_eq!(load_image(pid), expect(SyscallError::AccessDenied));
}
