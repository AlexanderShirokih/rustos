//! ID_AA64MMFR0_EL1 - read-only feature register.
//!
//! Используется для runtime-detect ширины ASID (`ASIDBits` поле `[7:4]`).

pub use hal_aarch64_asid::AsidWidth;

use crate::{memory::regs::common::EL1, read_sysreg};

/// Доступ к ID_AA64MMFR0_EL1.
pub struct IdAa64Mmfr0<EL> {
    _phantom: core::marker::PhantomData<EL>,
}

impl IdAa64Mmfr0<EL1> {
    /// Декодирует поле `ASIDBits[7:4]`. `0b0000` -> 8 бит, `0b0010` -> 16 бит;
    /// прочие значения reserved - трактуются как 8 бит (минимально гарантированное).
    pub fn asid_width() -> AsidWidth {
        // SAFETY: чтение feature-регистра ID_AA64MMFR0_EL1 на EL1 не имеет
        // побочных эффектов и разрешено без привилегированных проверок.
        let raw = unsafe { read_sysreg!(id_aa64mmfr0_el1) };
        match (raw >> 4) & 0b1111 {
            0b0010 => AsidWidth::Bits16,
            _ => AsidWidth::Bits8,
        }
    }
}
