//! Translation Control Register
//! Регистр, который описывает геометрию и правила трансляции адресов.
//!
use core::marker::PhantomData;

use crate::{
    combine_bits,
    memory::regs::{
        common::EL1,
        ttbr::{HigherHalf, LowerHalf},
    },
    write_sysreg,
};

// Поля TCR:
// ┌─────────────────────────────────────────────────────────────────┐
// │                         TCR_EL1                                 │
// ├─────────┬─────────┬─────────┬─────────┬─────────┬───────────────┤
// │ Биты    │ 5:0     │ 9:8     │ 11:10   │ 13:12   │ 15:14         │
// │ TTBR0   │ T0SZ    │ IRGN0   │ ORGN0   │ SH0     │ TG0           │
// ├─────────┼─────────┼─────────┼─────────┼─────────┼───────────────┤
// │ Биты    │ 21:16   │ 25:24   │ 27:26   │ 29:28   │ 31:30         │
// │ TTBR1   │ T1SZ    │ IRGN1   │ ORGN1   │ SH1     │ TG1           │
// └─────────┴─────────┴─────────┴─────────┴─────────┴───────────────┘

/// Селектор TTBR (lower/higher half).
pub trait TtbrSel {
    /// Сдвиг T0SZ/T1SZ.
    const TNSZ_SHIFT: u64;
    /// Сдвиг EPD (enable/disable).
    const EDP_SHIFT: u64;
    /// Сдвиг IRGN (inner cacheability).
    const IRGN_SHIFT: u64;
    /// Сдвиг ORGN (outer cacheability).
    const ORGN_SHIFT: u64;
    /// Сдвиг SH (shareability).
    const SH_SHIFT: u64;
    /// Сдвиг TG (granule size).
    const TG_SHIFT: u64;
    /// Значение для 4K страниц.
    const TG_4K_VALUE: u64;
}

// Биты TTBR0 (lower half):
impl TtbrSel for LowerHalf {
    const TNSZ_SHIFT: u64 = 0;
    const EDP_SHIFT: u64 = 7;
    const IRGN_SHIFT: u64 = 8;
    const ORGN_SHIFT: u64 = 10;
    const SH_SHIFT: u64 = 12;
    const TG_SHIFT: u64 = 14;
    const TG_4K_VALUE: u64 = 0b00;
}

// Биты TTBR1 (higher half):
impl TtbrSel for HigherHalf {
    const TNSZ_SHIFT: u64 = 16;
    const EDP_SHIFT: u64 = 23;
    const IRGN_SHIFT: u64 = 24;
    const ORGN_SHIFT: u64 = 26;
    const SH_SHIFT: u64 = 28;
    const TG_SHIFT: u64 = 30;
    const TG_4K_VALUE: u64 = 0b10;
}

/// Бит TCR.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct TCRBit(u64);

impl TCRBit {
    /// 48-битное адресное пространство (T0SZ/T1SZ = 16)
    pub const fn size_48bit<Sel: TtbrSel>() -> Self {
        Self(16 << Sel::TNSZ_SHIFT)
    }

    /// Write-Back Write-Allocate кэширование (IRGN)
    pub const fn irgn_wb_wa<Sel: TtbrSel>() -> Self {
        Self(0b01 << Sel::IRGN_SHIFT)
    }

    /// Write-Back Write-Allocate кэширование (ORGN)
    pub const fn orgn_wb_wa<Sel: TtbrSel>() -> Self {
        Self(0b01 << Sel::ORGN_SHIFT)
    }

    /// Inner Shareable
    pub const fn sh_inner<Sel: TtbrSel>() -> Self {
        Self(0b11 << Sel::SH_SHIFT)
    }

    /// 4K страницы
    pub const fn tg_4k<Sel: TtbrSel>() -> Self {
        Self(Sel::TG_4K_VALUE << Sel::TG_SHIFT)
    }

    pub const fn epd<Sel: TtbrSel>(enable: bool) -> Self {
        let bit = match enable {
            true => 0b0,
            false => 0b1 << Sel::EDP_SHIFT,
        };

        Self(bit)
    }

    /// IPS = 40-bit PA (1TB), биты `TCR[34:32]` - для MMIO адресов > 4GB
    pub const fn ips_40bit() -> Self {
        Self(0b010 << 32)
    }

    const fn encode(self) -> u64 {
        self.0
    }
}

/// Регистр TCR (Translation Control Register).
pub struct TranslationControlRegister<EL> {
    _phantom: PhantomData<EL>,
}

impl TranslationControlRegister<EL1> {
    pub const fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }

    pub fn set(
        &self,
        lower: AddressTranslationConfig<LowerHalf>,
        higher: AddressTranslationConfig<HigherHalf>,
    ) {
        // SAFETY: Запись в TCR_EL1 допустима на EL1 до включения MMU.
        // Значение формируется из типобезопасных конфигураций LowerHalf/HigherHalf.
        unsafe { write_sysreg!(tcr_el1, lower.value | higher.value) };
    }
}

/// Конфигурация трансляции адресов.
pub struct AddressTranslationConfig<T: TtbrSel> {
    /// Значение TCR.
    value: u64,
    _phantom: PhantomData<T>,
}

impl<T: TtbrSel> AddressTranslationConfig<T> {
    /// Объединяет биты TCR.
    pub const fn combine(bits: &[TCRBit]) -> Self {
        Self {
            value: combine_bits!(bits),
            _phantom: PhantomData,
        }
    }

    /// Создаёт стандартную конфигурацию.
    pub const fn create(enable: bool) -> Self {
        Self::combine(&[
            TCRBit::size_48bit::<T>(),
            TCRBit::irgn_wb_wa::<T>(),
            TCRBit::orgn_wb_wa::<T>(),
            TCRBit::sh_inner::<T>(),
            TCRBit::tg_4k::<T>(),
            TCRBit::epd::<T>(enable),
            TCRBit::ips_40bit(),
        ])
    }
}
