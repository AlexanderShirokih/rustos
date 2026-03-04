//! Translation Lookaside Buffer
//! Кэш трансляций VA -> PA, чтобы MMU не ходил каждый раз по page tables
//!
use crate::memory::regs::common::EL1;
use core::arch::asm;

/// TLB (Translation Lookaside Buffer).
pub struct TranslationLookasideBuffer<EL> {
    _phantom: core::marker::PhantomData<EL>,
}

impl TranslationLookasideBuffer<EL1> {
    pub const fn new() -> Self {
        Self {
            _phantom: core::marker::PhantomData,
        }
    }

    /// Инвалидирует все записи TLB.
    pub fn invalidate(&self) {
        unsafe {
            asm!(
                "dsb ishst",    // Ожидание записи page tables
                "tlbi vmalle1", // Инвалидация TLB
                "dsb ish",      // Ожидание завершения tlbi
                "isb",          // Синхронизация pipeline
                options(nostack, preserves_flags)
            )
        };
    }
}
