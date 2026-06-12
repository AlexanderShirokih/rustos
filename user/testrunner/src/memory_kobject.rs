//! E2E проверка `KObject::Memory` через `MemoryCreateVirtual` +
//! `MemoryMap` + `MemoryRegionInspect` из EL0.

use kernel_tests::kernel_test;
use userland_rt::{memory_create_virtual, memory_map, memory_region_inspect};

const PAGE_SIZE: u64 = 4096;
const PATTERN: u64 = 0xDEAD_BEEF_CAFE_BABE;
/// `access_mask`: биты R|W.
const ACCESS_RW: u64 = 0b11;
/// Secondary-возврат inspect'а: `(kind_tag << 16) | access_bits`,
/// kind_tag Virtual = 1.
const EXPECTED_INSPECT_SECONDARY: u64 = (1 << 16) | ACCESS_RW;

#[kernel_test]
fn memory_kobject_map_and_inspect() {
    let region_ret = memory_create_virtual(PAGE_SIZE, ACCESS_RW);
    kernel_tests::kassert!(region_ret > 0);
    let region = usize::try_from(region_ret).expect("positive handle fits usize");

    let va = memory_map(region, PAGE_SIZE, 0);
    kernel_tests::kassert!(va > 0);

    let slot = usize::try_from(va).expect("positive va fits usize") as *mut u64;
    // SAFETY: MemoryMap выдал RW-маппинг размером PAGE_SIZE; запись и чтение
    // первых 8 байт лежат в его границах.
    unsafe {
        slot.write_volatile(PATTERN);
        kernel_tests::kassert_eq!(slot.read_volatile(), PATTERN);
    }

    let (size, secondary) = memory_region_inspect(region);
    kernel_tests::kassert_eq!(size, i64::try_from(PAGE_SIZE).expect("page size fits i64"));
    kernel_tests::kassert_eq!(secondary, EXPECTED_INSPECT_SECONDARY);
}
