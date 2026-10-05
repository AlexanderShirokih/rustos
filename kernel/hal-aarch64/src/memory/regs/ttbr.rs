//! Translation Table Base Register.
//! Регистр, указывающий на корень page tables:
//! lower half -> TTBR0_EL1
//! higher half -> TTBR1_EL1
use core::marker::PhantomData;

use memory::physical_address::PhysicalAddress;

use crate::{
    memory::regs::common::{EL1, ExceptionLevel},
    write_sysreg,
};

const ASID_SHIFT: u64 = 48;

pub(crate) enum LowerHalf {}
pub(crate) enum HigherHalf {}

pub(crate) struct TranslationTableBaseRegister<EL: ExceptionLevel, TTBRIndex> {
    _phantom: PhantomData<(EL, TTBRIndex)>,
}

impl TranslationTableBaseRegister<EL1, LowerHalf> {
    pub(crate) fn set(root: PhysicalAddress) {
        // SAFETY: Запись в TTBR0_EL1 допустима на EL1. Адрес таблицы страниц
        // выровнен и корректен - формируется вызывающим кодом до включения MMU.
        unsafe { write_sysreg!(ttbr0_el1, root.as_usize() as u64) };
    }

    // SAFETY: `root` - корень живых таблиц страниц адресного пространства с ASID `asid`, либо обход
    // по TTBR0 запрещён битом `TCR_EL1.EPD0`.
    pub(crate) unsafe fn set_with_asid(root: PhysicalAddress, asid: u16) {
        // SAFETY: запись в TTBR0_EL1 допустима на EL1, значение задаёт контракт функции. `isb`
        // завершает запись до следующей инструкции.
        unsafe {
            write_sysreg!(ttbr0_el1, (u64::from(asid) << ASID_SHIFT) | root.as_u64());
            core::arch::asm!("isb", options(nostack, preserves_flags));
        }
    }
}

impl TranslationTableBaseRegister<EL1, HigherHalf> {
    pub(crate) fn set(root: PhysicalAddress) {
        // SAFETY: Запись в TTBR1_EL1 допустима на EL1. Адрес таблицы страниц
        // выровнен и корректен - формируется вызывающим кодом до включения MMU.
        unsafe { write_sysreg!(ttbr1_el1, root.as_usize() as u64) };
    }
}
