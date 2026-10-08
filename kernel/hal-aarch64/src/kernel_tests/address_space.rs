//! Per-process AddressSpace на стороне железа: `TTBR0_EL1` отслеживает
//! активное пространство.
#![allow(unsafe_code)]

extern crate alloc;

use kernelspace::syscall_bridge;
use scheduler::{AddressSpace, ArchContext};

use crate::sched::Aarch64Context;

const TTBR0_ROOT_MASK: u64 = 0x0000_FFFF_FFFF_F000;

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

#[test_harness::test]
fn ttbr0_tracks_active_address_space() {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    let user_as = AddressSpace::new_user(factory).expect("create user AS");
    let handle = user_as.handle().expect("user variant has handle");
    let root = handle.root.as_u64();

    Aarch64Context::switch_address_space(Some(handle));
    test_harness::check_eq!(read_ttbr0() & TTBR0_ROOT_MASK, root);

    // Возврат в kernel-AS идёт до drop: TLB держит трансляции освобождённого
    // user-AS до следующего switch.
    Aarch64Context::switch_address_space(None);
    test_harness::check_eq!(read_ttbr0(), 0);

    drop(user_as);
    test_harness::check_eq!(read_ttbr0(), 0);
}
