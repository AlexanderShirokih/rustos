//! ID_AA64MMFR0_EL1 - read-only feature register.
//!
//! Используется для runtime-detect ширины ASID (`ASIDBits` поле `[7:4]`).

pub(crate) use hal_aarch64_asid::AsidWidth;

use crate::{
    memory::regs::common::{EL1, ExceptionLevel},
    read_sysreg,
};

pub(crate) struct IdAa64Mmfr0<EL: ExceptionLevel> {
    _phantom: core::marker::PhantomData<EL>,
}

impl IdAa64Mmfr0<EL1> {
    pub(crate) fn asid_width() -> AsidWidth {
        // SAFETY: чтение feature-регистра ID_AA64MMFR0_EL1 на EL1 не имеет
        // побочных эффектов и разрешено без привилегированных проверок.
        let raw = unsafe { read_sysreg!(id_aa64mmfr0_el1) };
        match (raw >> 4) & 0b1111 {
            0b0010 => AsidWidth::Bits16,
            _ => AsidWidth::Bits8,
        }
    }
}
