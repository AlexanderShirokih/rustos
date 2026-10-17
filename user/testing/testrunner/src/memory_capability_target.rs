//! E2E проверка `CapabilityTarget::Memory` через типизированные обёртки
//! `Resource`/`MemoryRegion`/`Mapping` из EL0.

use kernel_objects::{
    Error, Mapping, MemoryRegion, MmioWindow, ReadOnlyMapping, RegionInfo, RegionKind, Resource,
    WriteCombiningMapping,
};
use runtime::{MemoryAccess, MemoryProfile, UserMemFlags, process_create};
use syscall::SyscallError;

use crate::filesystem::LEAK_ROUNDS;

const PAGE_SIZE: u64 = syscall::PAGE_SIZE as u64;
const PATTERN: u64 = 0xDEAD_BEEF_CAFE_BABE;

#[test_harness::test]
fn memory_capability_target_map_and_inspect() {
    let resource = Resource::self_resource().expect("self metering resource");
    let region = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("region");

    let mapping = Mapping::new(region, PAGE_SIZE, UserMemFlags::ReadWrite).expect("map");

    let slot = usize::try_from(mapping.va()).expect("positive va fits usize") as *mut u64;
    // SAFETY: map выдал RW-маппинг размером PAGE_SIZE; запись и чтение первых
    // 8 байт лежат в его границах.
    unsafe {
        slot.write_volatile(PATTERN);
        test_harness::check_eq!(slot.read_volatile(), PATTERN);
    }

    let info = mapping.region().inspect().expect("inspect");
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
fn a_write_combining_mapping_refuses_a_cached_region_and_returns_the_window() {
    let resource = Resource::self_resource().expect("self metering resource");

    for _ in 0..LEAK_ROUNDS {
        let window = MmioWindow {
            region: resource
                .create_virtual(PAGE_SIZE, MemoryAccess::RW)
                .expect("region"),
            offset: 0,
            size: PAGE_SIZE,
        };

        let refused = WriteCombiningMapping::new(window).expect_err("cached window refused");

        test_harness::check_eq!(refused.error, Error::Syscall(SyscallError::InvalidArgument));
        test_harness::check!(refused.region.region.inspect().is_ok());
    }
}

// Ядро отклоняет буфер syscall в отображении устройства. Кэшируемый доступ нарушает
// атрибуты этого отображения.
#[test_harness::test]
fn a_syscall_buffer_in_a_device_mapping_is_an_invalid_argument() {
    let Ok(window) = crate::hardware_session().client().acquire_device_memory(0) else {
        test_harness::skip!("the platform grants no device window");
    };
    let window = MemoryRegion::from(window);
    let page = window
        .slice(0, PAGE_SIZE, MemoryAccess::READ)
        .expect("first page of the window");
    let mapping = Mapping::new(page, PAGE_SIZE, UserMemFlags::ReadOnly).expect("map window");

    let base = usize::try_from(mapping.va()).expect("positive va fits usize") as *const u8;
    // SAFETY: маппинг разрешает чтение страницы окна на всё время жизни среза.
    // `process_create` передаёт ядру адрес и длину. Код EL0 не читает содержимое среза.
    let name = unsafe { core::slice::from_raw_parts(base, 8) };

    test_harness::check_eq!(
        process_create(name).map(drop),
        Err(SyscallError::InvalidArgument)
    );
}

#[test_harness::test]
fn a_mapping_outlives_the_function_that_built_it() {
    let mapping = mapped_page();

    mapping.copy_in(&PATTERN.to_ne_bytes()).expect("write");
    let mut read = [0; 8];
    mapping.copy_out(&mut read).expect("read");

    test_harness::check_eq!(u64::from_ne_bytes(read), PATTERN);
}

#[test_harness::test]
fn a_mapping_returns_its_region_open() {
    let mapping = mapped_page();
    mapping.copy_in(&PATTERN.to_ne_bytes()).expect("write");

    let region = mapping.into_region();
    let remapped = Mapping::full(region, UserMemFlags::ReadOnly).expect("region maps again");
    let mut read = [0; 8];
    remapped.copy_out(&mut read).expect("read");

    test_harness::check_eq!(u64::from_ne_bytes(read), PATTERN);
}

#[test_harness::test]
fn mapping_and_releasing_an_own_region_does_not_exhaust_the_handle_table() {
    for _ in 0..LEAK_ROUNDS {
        drop(mapped_page());
    }
}

#[test_harness::test]
fn a_writable_region_refused_read_only_comes_back_mappable_for_writing() {
    let region = page_region();

    let refused = ReadOnlyMapping::new(region).expect_err("writable region refused");
    let writable = Mapping::full(refused.region, UserMemFlags::ReadWrite);

    test_harness::check_eq!(refused.error, Error::Syscall(SyscallError::AccessDenied));
    test_harness::check!(writable.is_ok());
}

#[test_harness::test]
fn a_refused_mapping_does_not_exhaust_the_handle_table() {
    for _ in 0..LEAK_ROUNDS {
        let refused = ReadOnlyMapping::new(page_region()).expect_err("writable region refused");

        test_harness::check_eq!(refused.error, Error::Syscall(SyscallError::AccessDenied));
    }
}

#[test_harness::test]
fn a_temporary_mapping_leaves_its_region_open() {
    let region = page_region();

    let first = Mapping::full(&region, UserMemFlags::ReadWrite).expect("first mapping");
    first.copy_in(&PATTERN.to_ne_bytes()).expect("write");
    drop(first);
    let second = Mapping::full(&region, UserMemFlags::ReadOnly).expect("second mapping");
    let mut read = [0; 8];
    second.copy_out(&mut read).expect("read");

    test_harness::check_eq!(u64::from_ne_bytes(read), PATTERN);
    test_harness::check!(region.inspect().is_ok());
}

fn page_region() -> MemoryRegion {
    Resource::self_resource()
        .expect("self metering resource")
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("region")
}

fn mapped_page() -> Mapping {
    Mapping::full(page_region(), UserMemFlags::ReadWrite).expect("map")
}
