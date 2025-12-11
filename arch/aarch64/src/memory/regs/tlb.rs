//! Translation Lookaside Buffer
//! Кэш трансляций VA → PA, чтобы MMU не ходил каждый раз по page tables
//!
use crate::memory::regs::common::EL1;
use core::arch::asm;

pub struct TranslationLookasideBuffer<EL> {
    _phantom: core::marker::PhantomData<EL>,
}

impl TranslationLookasideBuffer<EL1> {
    pub const fn new() -> Self {
        Self {
            _phantom: core::marker::PhantomData,
        }
    }

    pub fn invalidate(&self) {
        unsafe { asm!("dsb ish", "tlbi vmalle1", options(nostack, preserves_flags)) };
    }
}
