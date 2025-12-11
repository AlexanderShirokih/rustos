//! Translation Control Register
//! Регистр, который описывает геометрию и правила трансляции адресов.
//!
use crate::combine_bits;
use crate::memory::regs::common::EL1;
use core::arch::asm;

// TCR_EL1 fields (мы используем только TTBR0_EL1).
pub const TCR_T0SZ_48BIT: TCRBit = TCRBit(16u64 << 0); // 64-48
pub const TCR_IRGN0_WB_WA: TCRBit = TCRBit(0b01u64 << 8);
pub const TCR_ORGN0_WB_WA: TCRBit = TCRBit(0b01u64 << 10);
pub const TCR_SH0_INNER: TCRBit = TCRBit(0b11u64 << 12);
pub const TCR_TG0_4K: TCRBit = TCRBit(0b00u64 << 14);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TCRBit(u64);

impl TCRBit {
    const fn encode(self) -> u64 {
        self.0
    }
}

pub struct TranslationControlRegister<EL> {
    _phantom: core::marker::PhantomData<EL>,
}

impl TranslationControlRegister<EL1> {
    pub const fn new() -> Self {
        Self {
            _phantom: core::marker::PhantomData,
        }
    }

    pub fn set(&self, value: TCRBits) {
        unsafe { asm!("msr tcr_el1, {0}", in(reg) value.0, options(nostack, preserves_flags)) };
    }
}

pub struct TCRBits(u64);

impl TCRBits {
    pub const fn combine(bits: &[TCRBit]) -> TCRBits {
        TCRBits(combine_bits!(bits))
    }
}
