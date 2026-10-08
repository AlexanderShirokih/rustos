//! E2E проверка `CapabilityTarget::Memory` через типизированные обёртки
//! `Resource`/`MemoryRegion`/`Mapping` из EL0.

use kernel_objects::{Error, MmioWindow, RegionInfo, RegionKind, Resource};
use runtime::{MemoryAccess, MemoryProfile, UserMemFlags};
use syscall::SyscallError;

const PAGE_SIZE: u64 = syscall::PAGE_SIZE as u64;
const PATTERN: u64 = 0xDEAD_BEEF_CAFE_BABE;

#[test_harness::test]
fn memory_capability_target_map_and_inspect() {
    let resource = Resource::self_resource().expect("self metering resource");
    let region = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("region");

    let mapping = region.map(PAGE_SIZE, UserMemFlags::ReadWrite).expect("map");

    let slot = usize::try_from(mapping.va()).expect("positive va fits usize") as *mut u64;
    // SAFETY: map выдал RW-маппинг размером PAGE_SIZE; запись и чтение первых
    // 8 байт лежат в его границах.
    unsafe {
        slot.write_volatile(PATTERN);
        test_harness::check_eq!(slot.read_volatile(), PATTERN);
    }

    let info = region.inspect().expect("inspect");
    test_harness::check_eq!(
        info,
        RegionInfo {
            size_bytes: PAGE_SIZE,
            base: 0,
            kind: RegionKind::Virtual,
            profile: MemoryProfile::Cached,
            access: MemoryAccess::RW,
        }
    );
}

#[test_harness::test]
fn contiguous_region_reports_its_physical_base() {
    let resource = Resource::self_resource().expect("self metering resource");
    let region = resource
        .create_contiguous(2 * PAGE_SIZE, MemoryAccess::RW)
        .expect("contiguous region");

    let info = region.inspect().expect("inspect region");

    test_harness::check!(info.base != 0);
}

#[test_harness::test]
fn a_write_combining_mapping_refuses_a_cached_region() {
    let resource = Resource::self_resource().expect("self metering resource");
    let window = MmioWindow {
        region: resource
            .create_virtual(PAGE_SIZE, MemoryAccess::RW)
            .expect("region"),
        offset: 0,
        size: PAGE_SIZE,
    };

    test_harness::check!(matches!(
        window.map_write_combining(),
        Err(Error::Syscall(SyscallError::InvalidArgument))
    ));
}
