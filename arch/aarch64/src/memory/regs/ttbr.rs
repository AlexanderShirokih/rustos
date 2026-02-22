//! Translation Table Base Register.
//! Регистр, указывающий на корень page tables:
//! lower half → TTBR0_EL1
//! higher half → TTBR1_EL1
use crate::memory::regs::common::EL1;
use crate::write_sysreg;
use core::marker::PhantomData;
use memory::physical_address::PhysicalAddress;

/// Маркер lower half (TTBR0).
pub enum LowerHalf {}
/// Маркер higher half (TTBR1).
pub enum HigherHalf {}

/// Регистр TTBR (Translation Table Base Register).
pub struct TranslationTableBaseRegister<EL, TTBRIndex> {
    _phantom: PhantomData<(EL, TTBRIndex)>,
}

impl<EL, TTBRIndex> TranslationTableBaseRegister<EL, TTBRIndex> {
    pub const fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl TranslationTableBaseRegister<EL1, LowerHalf> {
    pub fn set(&self, root: PhysicalAddress) {
        // SAFETY: Запись в TTBR0_EL1 допустима на EL1. Адрес таблицы страниц
        // выровнен и корректен — формируется вызывающим кодом до включения MMU.
        unsafe { write_sysreg!(ttbr0_el1, root.as_usize() as u64) };
    }
}

impl TranslationTableBaseRegister<EL1, HigherHalf> {
    pub fn set(&self, root: PhysicalAddress) {
        // SAFETY: Запись в TTBR1_EL1 допустима на EL1. Адрес таблицы страниц
        // выровнен и корректен — формируется вызывающим кодом до включения MMU.
        unsafe { write_sysreg!(ttbr1_el1, root.as_usize() as u64) };
    }
}
