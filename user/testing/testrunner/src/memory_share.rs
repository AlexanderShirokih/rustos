//! Передача capability разделяемой памяти между процессами и жизнь её отображений.

use kernel_objects::{Mapping, Resource};
use runtime::{MemoryAccess, UserMemFlags, signal_set};
use syscall::{Handle, Rights, SIGNALED, SyscallError, WakeCount, kind};

use crate::{
    fixture::Fixture,
    fixture_protocol::{OP_MAP_CHECK, REPLY_OK},
};

const PAGE_SIZE: u64 = syscall::PAGE_SIZE as u64;
const PATTERN: u64 = 0x5EED_F00D_C0DE_AB1E;

#[test_harness::test]
fn memory_region_shared_with_child() {
    let resource = Resource::self_resource().expect("self metering resource");
    let region = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("create shared region");
    let mapping =
        Mapping::new(region, PAGE_SIZE, UserMemFlags::ReadWrite).expect("map region in parent");

    let slot = usize::try_from(mapping.va()).expect("positive va fits usize") as *mut u64;
    // SAFETY: map выдал RW-окно в PAGE_SIZE байт; 8-байтные доступы по его базе
    // лежат в границах, а другие владельцы у маппинга отсутствуют.
    unsafe {
        slot.write_volatile(PATTERN);
    }

    let fixture = Fixture::spawn();

    let lease = mapping
        .region()
        .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
        .expect("duplicate region handle for child");
    let leased_raw = lease.as_raw();

    let status = fixture.request_status(OP_MAP_CHECK, PATTERN, &[leased_raw.raw()]);
    test_harness::check_eq!(status, REPLY_OK);

    let stale = Handle::<kind::Signal>::from_abi(lease.into_raw().as_nonzero());
    test_harness::check_eq!(
        signal_set(stale, SIGNALED, 0, WakeCount::None),
        SyscallError::BadHandle.as_return_value()
    );

    // SAFETY: см. выше.
    unsafe {
        test_harness::check_eq!(slot.read_volatile(), !PATTERN);
    }
}

#[test_harness::test]
fn mapping_drop_after_revocation_preserves_another_mapping() {
    let resource = Resource::self_resource().expect("resource");
    let source = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("source");
    let lease = source
        .duplicate(Rights::READ | Rights::WRITE, None)
        .expect("lease");
    let revoked = Mapping::full(lease, UserMemFlags::ReadWrite).expect("first mapping");
    drop(source);
    test_harness::check_eq!(
        revoked.remap(UserMemFlags::ReadOnly),
        Err(kernel_objects::Error::Syscall(SyscallError::Revoked))
    );
    let replacement = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("replacement");
    let replacement =
        Mapping::full(replacement, UserMemFlags::ReadWrite).expect("replacement mapping");
    replacement.copy_in(b"live").expect("write replacement");
    drop(revoked);
    let mut bytes = [0; 4];
    replacement
        .copy_out(&mut bytes)
        .expect("read replacement after stale drop");
    test_harness::check_eq!(&bytes, b"live");
}
