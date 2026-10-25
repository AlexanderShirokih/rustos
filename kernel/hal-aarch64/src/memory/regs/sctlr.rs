//! System Control Register (SCTLR).
//!
//! Управляет MMU, кэшами и проверкой выравнивания обращений к памяти.

use crate::{
    combine_bits,
    memory::regs::common::{EL1, ExceptionLevel},
    read_sysreg, write_sysreg,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SctlrBit {
    Mmu,
    AlignmentCheck,
    DCache,
    ICache,
}

impl SctlrBit {
    const fn encode(self) -> u64 {
        match self {
            SctlrBit::Mmu => 1 << 0,
            SctlrBit::AlignmentCheck => 1 << 1,
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

    #[cfg(feature = "kernel-tests")]
    pub(crate) const fn contains(self, bit: SctlrBit) -> bool {
        self.0 & bit.encode() != 0
    }
}

pub(crate) struct SystemControlRegister<EL: ExceptionLevel> {
    _phantom: core::marker::PhantomData<EL>,
}

impl SystemControlRegister<EL1> {
    #[cfg(feature = "kernel-tests")]
    pub(crate) fn read() -> SctlrBits {
        // SAFETY: Чтение SCTLR_EL1 допустимо на EL1 и не меняет состояние.
        SctlrBits(unsafe { read_sysreg!(sctlr_el1) })
    }

    pub(crate) fn update(enable: SctlrBits, disable: SctlrBits) {
        // SAFETY: Чтение и запись SCTLR_EL1 допустимы на EL1. Новое значение берёт текущее,
        // снимает биты `disable` и ставит биты `enable`. Остальные биты сохраняются, и
        // MMU-инварианты не нарушаются.
        unsafe {
            let value = read_sysreg!(sctlr_el1);
            write_sysreg!(sctlr_el1, (value & !disable.0) | enable.0);
        }
    }
}
