//! System Control Register (SCTLR).
//!
//! Управляет MMU и кэшами.

use crate::combine_bits;
use crate::memory::regs::common::EL1;
use core::arch::asm;

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
    pub const fn new() -> Self {
        Self {
            _phantom: core::marker::PhantomData,
        }
    }

    pub fn set(&self, mask: SctlrBits) {
        unsafe {
            let value: u64;
            asm!("mrs {0}, sctlr_el1", out(reg) value, options(nostack, preserves_flags));
            asm!("msr sctlr_el1, {0}", in(reg) value| mask.0, options(nostack, preserves_flags));
        };
    }
}
