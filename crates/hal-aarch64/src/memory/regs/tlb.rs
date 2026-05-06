//! Translation Lookaside Buffer
//! Кэш трансляций VA -> PA, чтобы MMU не ходил каждый раз по page tables
//!
use core::arch::asm;

use crate::memory::regs::common::EL1;

/// TLB (Translation Lookaside Buffer).
pub struct TranslationLookasideBuffer<EL> {
    _phantom: core::marker::PhantomData<EL>,
}

impl TranslationLookasideBuffer<EL1> {
    /// Инвалидирует все записи TLB.
    pub fn invalidate() {
        // SAFETY: `dsb`/`tlbi vmalle1`/`isb` - стандартная последовательность инвалидации TLB
        // на EL1, не имеет операндов, не модифицирует регистры (preserves_flags) и память.
        unsafe {
            asm!(
                "dsb ishst",    // Ожидание записи page tables
                "tlbi vmalle1", // Инвалидация TLB
                "dsb ish",      // Ожидание завершения tlbi
                "isb",          // Синхронизация pipeline
                options(nostack, preserves_flags),
            );
        }
    }
}
