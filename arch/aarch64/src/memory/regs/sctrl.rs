use crate::memory::regs::common::EL1;
use crate::{combine_bits, system};
use core::arch::asm;

// Главный регистр управляющий MMU/кэшами

/// Биты, которые мы выставляем в `SCTLR` при включении MMU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SctlrBit {
    /// Включить MMU.
    MmuEnable,
    /// Включить D-cache.
    DCacheEnable,
    /// Включить I-cache.
    ICacheEnable,
}

impl SctlrBit {
    const fn encode(self) -> u64 {
        match self {
            SctlrBit::MmuEnable => 1 << 0,
            SctlrBit::DCacheEnable => 1 << 2,
            SctlrBit::ICacheEnable => 1 << 12,
        }
    }
}

pub struct SctlrBits(u64);

impl SctlrBits {
    pub const fn combine(bits: &[SctlrBit]) -> SctlrBits {
        SctlrBits(combine_bits!(bits))
    }
}

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

        system::barrier::barrier();
    }
}
