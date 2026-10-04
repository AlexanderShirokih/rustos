//! E2E проверка VM-syscall из EL0: `MemoryAllocate`, `MemoryRemap`,
//! `MemoryFree`.
//!
//! Раньше эти сценарии жили kernel-тестами, где payload собирался машинными
//! словами и запускался отдельным user-процессом. Из EL0 они выражаются
//! прямыми вызовами, а проверка становится сильнее: kernel-тест довольствовался
//! фактом прихода сигнала, здесь наблюдаются и выданный VA, и содержимое
//! страницы.

use kernel_objects::Resource;
use kernel_tests::kernel_test;
use runtime::UserMemFlags;

const PAGE_SIZE: u64 = syscall::PAGE_SIZE as u64;
const PATTERN: u64 = 0xC0DE_F00D_DEAD_BEEF;

#[kernel_test]
fn allocate_write_then_remap_readonly() {
    let resource = Resource::self_resource().expect("self metering resource");
    let mapping = resource
        .allocate(PAGE_SIZE, UserMemFlags::ReadWrite)
        .expect("allocate one page");

    let slot = usize::try_from(mapping.va()).expect("positive va fits usize") as *mut u64;
    // SAFETY: allocate выдал RW-страницу PAGE_SIZE байт; 8-байтные доступы по
    // её базе лежат в границах, а другие владельцы отсутствуют.
    unsafe {
        slot.write_volatile(PATTERN);
        kernel_tests::kassert_eq!(slot.read_volatile(), PATTERN);
    }

    mapping
        .remap(UserMemFlags::ReadOnly)
        .expect("remap to read-only");

    // SAFETY: ReadOnly сохраняет право чтения по тому же VA.
    unsafe {
        kernel_tests::kassert_eq!(slot.read_volatile(), PATTERN);
    }
}

#[kernel_test]
fn allocate_free_reuses_va() {
    let resource = Resource::self_resource().expect("self metering resource");

    let first = resource
        .allocate(PAGE_SIZE, UserMemFlags::ReadWrite)
        .expect("first allocate");
    let va = first.va();

    let slot = usize::try_from(va).expect("positive va fits usize") as *mut u64;
    // SAFETY: см. `allocate_write_then_remap_readonly`.
    unsafe {
        slot.write_volatile(PATTERN);
    }

    first.free().expect("free the first region");

    let second = resource
        .allocate(PAGE_SIZE, UserMemFlags::ReadWrite)
        .expect("second allocate");
    kernel_tests::kassert_eq!(second.va(), va);

    // SAFETY: см. выше.
    unsafe {
        kernel_tests::kassert_eq!(slot.read_volatile(), 0);
    }
}
