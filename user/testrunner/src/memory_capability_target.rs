//! E2E проверка `CapabilityTarget::Memory` через типизированные обёртки
//! `Resource`/`MemoryRegion`/`Mapping` из EL0.

use kernel_tests::kernel_test;
use runtime::{MemoryAccess, RegionInfo, RegionKind, Resource, UserMemFlags};

const PAGE_SIZE: u64 = 4096;
const PATTERN: u64 = 0xDEAD_BEEF_CAFE_BABE;

#[kernel_test]
fn memory_capability_target_map_and_inspect() {
    let resource = Resource::self_resource().expect("metering resource");
    let region = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("region");

    let mapping = region.map(PAGE_SIZE, UserMemFlags::ReadWrite).expect("map");

    let slot = usize::try_from(mapping.va()).expect("positive va fits usize") as *mut u64;
    // SAFETY: map выдал RW-маппинг размером PAGE_SIZE; запись и чтение первых
    // 8 байт лежат в его границах.
    unsafe {
        slot.write_volatile(PATTERN);
        kernel_tests::kassert_eq!(slot.read_volatile(), PATTERN);
    }

    let info = region.inspect().expect("inspect");
    kernel_tests::kassert_eq!(
        info,
        RegionInfo {
            size_bytes: PAGE_SIZE,
            base: 0,
            kind: RegionKind::Virtual,
            access: MemoryAccess::RW,
        }
    );
}
