//! System Control Register (SCTLR).
//!
//! Управляет MMU и кэшами.

use crate::{combine_bits, memory::regs::common::EL1, read_sysreg, write_sysreg};

/// Бит SCTLR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SctlrBit {
    /// Включить MMU.
    Mmu,
    /// Включить D-cache.
    DCache,
    /// Включить I-cache.
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

/// Скомбинированные биты SCTLR.
#[derive(Debug, Clone, Copy)]
pub struct SctlrBits(u64);

impl SctlrBits {
    /// Объединяет биты SCTLR.
    pub const fn combine(bits: &[SctlrBit]) -> SctlrBits {
        SctlrBits(combine_bits!(bits))
    }
}

/// Регистр SCTLR (System Control Register).
pub struct SystemControlRegister<EL> {
    _phantom: core::marker::PhantomData<EL>,
}

impl SystemControlRegister<EL1> {
    pub fn set(mask: SctlrBits) {
        // SAFETY: Чтение и запись SCTLR_EL1 допустимы на EL1. Новое значение
        // формируется как OR текущего значения и маски - MMU-инварианты не нарушаются.
        unsafe {
            let value = read_sysreg!(sctlr_el1);
            write_sysreg!(sctlr_el1, value | mask.0);
        }
    }
}
