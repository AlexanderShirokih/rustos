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
        unsafe {
            asm!(
                "dsb ishst",      // Убедиться, что page tables записаны
                "tlbi vmalle1",   // Инвалидировать TLB
                "dsb ish",        // Убедиться, что tlbi завершён
                "isb",            // Синхронизировать pipeline
                options(nostack, preserves_flags)
            )
        };
    }
}
