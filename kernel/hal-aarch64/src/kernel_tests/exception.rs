//! Платформенная проверка классификации источника исключения по SPSR.

use kernel_tests::kernel_test;

use crate::exception::originates_from_el0;

#[kernel_test]
fn exception_origin_classifies_el0_and_el1_modes() {
    const AARCH64_EL0T: u64 = 0b0000;
    const AARCH64_EL1T: u64 = 0b0100;
    const AARCH64_EL1H: u64 = 0b0101;
    const AARCH32_USER: u64 = 0b1_0000;
    const SPSR_FLAGS: u64 = 0xF000_03C0;

    kernel_tests::kassert!(originates_from_el0(AARCH64_EL0T));
    kernel_tests::kassert!(originates_from_el0(AARCH64_EL0T | SPSR_FLAGS));
    kernel_tests::kassert!(originates_from_el0(AARCH32_USER));
    kernel_tests::kassert!(!originates_from_el0(AARCH64_EL1T));
    kernel_tests::kassert!(!originates_from_el0(AARCH64_EL1H));
}
