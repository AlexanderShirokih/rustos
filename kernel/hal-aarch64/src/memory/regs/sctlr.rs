//! System Control Register (SCTLR).
//!
//! Управляет MMU и кэшами.

use crate::{
    combine_bits,
    memory::regs::common::{EL1, ExceptionLevel},
    read_sysreg, write_sysreg,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SctlrBit {
    Mmu,
    DCache,
    ICache,
}

impl SctlrBit {
    const fn encode(self) -> u64 {
        match self {
            SctlrBit::Mmu => 1 << 0,
            SctlrBit::DCache => 1 << 2,
            SctlrBit::ICache => 1 << 12,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SctlrBits(u64);

impl SctlrBits {
    pub(crate) const fn combine(bits: &[SctlrBit]) -> SctlrBits {
        SctlrBits(combine_bits!(bits))
    }
}

pub(crate) struct SystemControlRegister<EL: ExceptionLevel> {
    _phantom: core::marker::PhantomData<EL>,
}

impl SystemControlRegister<EL1> {
    pub(crate) fn set(mask: SctlrBits) {
        // SAFETY: Чтение и запись SCTLR_EL1 допустимы на EL1. Новое значение
        // формируется как OR текущего значения и маски - MMU-инварианты не нарушаются.
        unsafe {
            let value = read_sysreg!(sctlr_el1);
            write_sysreg!(sctlr_el1, value | mask.0);
        }
    }
}
