//! Проверка выравнивания обращений после включения MMU.
#![allow(unsafe_code)]

use crate::memory::regs::{
    common::EL1,
    sctlr::{SctlrBit, SystemControlRegister},
};

#[test_harness::test]
fn alignment_check_is_off_after_mmu_enable() {
    let control = SystemControlRegister::<EL1>::read();
    test_harness::check!(!control.contains(SctlrBit::AlignmentCheck));
}

#[test_harness::test]
fn unaligned_load_from_normal_memory_returns_its_bytes() {
    let bytes: [u8; 16] = [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f,
    ];
    let expected = u64::from_le_bytes([0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18]);

    let loaded: u64;
    // SAFETY: адрес `bytes + 1` и следующие восемь байт лежат внутри массива на стеке, в обычной
    // памяти ядра. Инструкция только читает их.
    unsafe {
        core::arch::asm!(
            "ldr {loaded}, [{address}]",
            loaded = out(reg) loaded,
            address = in(reg) bytes.as_ptr().add(1),
            options(nostack, readonly, preserves_flags),
        );
    }

    test_harness::check_eq!(loaded, expected);
}
