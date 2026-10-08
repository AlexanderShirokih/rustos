//! Платформенная проверка классификации источника исключения по SPSR.

use crate::exception::originates_from_el0;

#[test_harness::test]
fn exception_origin_classifies_el0_and_el1_modes() {
    const AARCH64_EL0T: u64 = 0b0000;
    const AARCH64_EL1T: u64 = 0b0100;
    const AARCH64_EL1H: u64 = 0b0101;
    const AARCH32_USER: u64 = 0b1_0000;
    const SPSR_FLAGS: u64 = 0xF000_03C0;

    test_harness::check!(originates_from_el0(AARCH64_EL0T));
    test_harness::check!(originates_from_el0(AARCH64_EL0T | SPSR_FLAGS));
    test_harness::check!(originates_from_el0(AARCH32_USER));
    test_harness::check!(!originates_from_el0(AARCH64_EL1T));
    test_harness::check!(!originates_from_el0(AARCH64_EL1H));
}
