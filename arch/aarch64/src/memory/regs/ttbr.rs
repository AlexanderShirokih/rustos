//! Translation Table Base Register.
//! Регистр, укзаывающий на корень page tables:
//! VA
//! ├── lower half → TTBR0_EL1
//! └── higher half → TTBR1_EL1
use crate::memory::regs::common::EL1;
use core::arch::asm;
use core::marker::PhantomData;
use memory::physical::PhysicalAddress;

pub struct TTBR0;

pub struct TranslationTableBaseRegister<EL, TTBRIndex> {
    _phantom: PhantomData<EL>,
    _ttbr: PhantomData<TTBRIndex>,
}

impl<EL, TTBRIndex> TranslationTableBaseRegister<EL, TTBRIndex> {
    pub const fn new() -> Self {
        Self {
            _phantom: PhantomData,
            _ttbr: PhantomData,
        }
    }
}

impl TranslationTableBaseRegister<EL1, TTBR0> {
    pub fn set(&self, root: PhysicalAddress) {
        unsafe {
            asm!("msr ttbr0_el1, {0}", in(reg) root.as_usize() as u64, options(nostack, preserves_flags))
        };
    }
}
