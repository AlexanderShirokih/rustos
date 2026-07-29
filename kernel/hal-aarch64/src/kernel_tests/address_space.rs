//! Per-process AddressSpace на стороне железа: `TTBR0_EL1` отслеживает
//! активное пространство.
#![allow(unsafe_code)]

extern crate alloc;

use kernel_tests::kernel_test;
use kernelspace::syscall_bridge;
use scheduler::{AddressSpace, ArchContext};

use crate::sched::Aarch64Context;

/// Маска PA корневой таблицы в `TTBR0_EL1`: `[47:12]`, выше - ASID.
const TTBR0_ROOT_MASK: u64 = 0x0000_FFFF_FFFF_F000;

/// Читает `TTBR0_EL1`.
fn read_ttbr0() -> u64 {
    let raw: u64;
    // SAFETY: чтение TTBR0_EL1 на EL1 разрешено и безопасно.
    unsafe {
        core::arch::asm!(
            "mrs {raw}, ttbr0_el1",
            raw = out(reg) raw,
            options(nostack, nomem, preserves_flags),
        );
    }
    raw
}

/// `switch_address_space` кладёт в `TTBR0_EL1` корень активного user-AS,
/// возврат в kernel-AS обнуляет регистр, а drop AS после возврата не
/// оставляет регистр указывающим на освобождённую память.
#[kernel_test]
fn ttbr0_tracks_active_address_space() {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    let user_as = AddressSpace::new_user(factory).expect("create user AS");
    let handle = user_as.handle().expect("user variant has handle");
    let root = handle.root.as_u64();

    Aarch64Context::switch_address_space(Some(handle));
    kernel_tests::kassert_eq!(read_ttbr0() & TTBR0_ROOT_MASK, root);

    // Возврат в kernel-AS до drop'а: иначе TLB удержит трансляции из
    // освобождённого user-AS до следующего switch'а.
    Aarch64Context::switch_address_space(None);
    kernel_tests::kassert_eq!(read_ttbr0(), 0);

    drop(user_as);
    kernel_tests::kassert_eq!(read_ttbr0(), 0);
}
