//! E2E проверка `CapabilityTarget::Memory` через типизированные обёртки
//! `Resource`/`MemoryRegion`/`Mapping` из EL0.

use bootstrap_abi::bootstrap;
use kernel_tests::kernel_test;
use runtime::{
    MemoryAccess, MemoryProfile, MemoryRegion, PortTransport, RegionInfo, RegionKind, Resource,
    UserMemFlags,
};

const PAGE_SIZE: u64 = syscall::PAGE_SIZE as u64;
const PATTERN: u64 = 0xDEAD_BEEF_CAFE_BABE;

#[kernel_test]
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
        kernel_tests::kassert_eq!(slot.read_volatile(), PATTERN);
    }

    let info = region.inspect().expect("inspect");
    kernel_tests::kassert_eq!(
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

// Regression guard: фреймы кэшируемого региона несёт линейная карта ядра, и
// второе отображение с иным кэшированием теряет когерентность.
#[kernel_test]
fn memory_with_profile_refuses_a_cached_region() {
    let resource = Resource::self_resource().expect("self metering resource");
    let region = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("region");

    for profile in [MemoryProfile::WriteCombining, MemoryProfile::Device] {
        let outcome = region.with_profile(profile);
        kernel_tests::kassert!(outcome.is_err());
    }

    kernel_tests::kassert_eq!(
        region.inspect().expect("inspect").profile,
        MemoryProfile::Cached
    );
}

#[kernel_test]
fn memory_with_profile_retypes_a_device_window() {
    let client = bootstrap::Client::new(PortTransport::client(crate::bootstrap_handle()));
    let cap = client
        .acquire_device_memory(0)
        .expect("acquire_device_memory call")
        .expect("vend ok");
    let window = MemoryRegion::adopt(cap);

    let device = window.inspect().expect("inspect device window");
    kernel_tests::kassert_eq!(device.kind, RegionKind::Physical);
    kernel_tests::kassert_eq!(device.profile, MemoryProfile::Device);

    let combining = window
        .with_profile(MemoryProfile::WriteCombining)
        .expect("device window accepts write-combining");
    let retyped = combining.inspect().expect("inspect retyped window");
    kernel_tests::kassert_eq!(retyped.profile, MemoryProfile::WriteCombining);
    kernel_tests::kassert_eq!(retyped.base, device.base);
    kernel_tests::kassert_eq!(retyped.size_bytes, device.size_bytes);

    let mapping = combining
        .map_full(UserMemFlags::ReadWrite)
        .expect("map retyped window");
    kernel_tests::kassert_eq!(mapping.size_bytes(), device.size_bytes);

    kernel_tests::kassert!(combining.with_profile(MemoryProfile::Cached).is_err());
}
