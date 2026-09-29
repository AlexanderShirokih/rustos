//! Translation Control Register
//! Регистр, который описывает геометрию и правила трансляции адресов.
//!
use core::marker::PhantomData;

use crate::{
    combine_bits,
    memory::regs::{
        common::{EL1, ExceptionLevel},
        id_aa64mmfr0::AsidWidth,
        ttbr::{HigherHalf, LowerHalf},
    },
    read_sysreg, write_sysreg,
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
    const EPD_SHIFT: u64;
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
    const EPD_SHIFT: u64 = 7;
    const IRGN_SHIFT: u64 = 8;
    const ORGN_SHIFT: u64 = 10;
    const SH_SHIFT: u64 = 12;
    const TG_SHIFT: u64 = 14;
    const TG_4K_VALUE: u64 = 0b00;
}

// Биты TTBR1 (higher half):
impl TtbrSel for HigherHalf {
    const TNSZ_SHIFT: u64 = 16;
    const EPD_SHIFT: u64 = 23;
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
        let bit = if enable { 0b0 } else { 0b1 << Sel::EPD_SHIFT };

        Self(bit)
    }

    /// IPS = 40-bit PA (1TB), биты `TCR[34:32]` - для MMIO адресов > 4GB
    pub const fn ips_40bit() -> Self {
        Self(0b010 << 32)
    }

    /// Бит `TCR_EL1.AS` (bit 36): `0` - 8-bit ASID, `1` - 16-bit.
    pub const fn asid_size(width: AsidWidth) -> Self {
        match width {
            AsidWidth::Bits8 => Self(0),
            AsidWidth::Bits16 => Self(1 << 36),
        }
    }

    const fn encode(self) -> u64 {
        self.0
    }
}

/// Регистр TCR (Translation Control Register).
pub struct TranslationControlRegister<EL: ExceptionLevel> {
    _phantom: PhantomData<EL>,
}

impl TranslationControlRegister<EL1> {
    pub fn set(
        lower: AddressTranslationConfig<LowerHalf>,
        higher: AddressTranslationConfig<HigherHalf>,
    ) {
        // SAFETY: Запись в TCR_EL1 допустима на EL1 до включения MMU.
        // Значение формируется из типобезопасных конфигураций LowerHalf/HigherHalf.
        unsafe { write_sysreg!(tcr_el1, lower.value | higher.value) };
    }

    /// Запрещает обход таблиц по TTBR половины `T`. Промах TLB в этой половине даёт Translation
    /// Fault уровня 0 без обращения к памяти.
    ///
    /// # Safety
    ///
    /// Другой код на этом CPU не пишет TCR_EL1 между чтением и записью, например IRQ замаскированы.
    pub unsafe fn disable_walks<T: TtbrSel>() {
        let epd = TCRBit::epd::<T>(false).encode();

        // SAFETY: чтение и запись TCR_EL1 допустимы на EL1. Запись меняет только бит EPD
        // половины `T`, а контракт функции исключает запись в TCR_EL1 между `mrs` и `msr`.
        // `isb` применяет запись до следующей инструкции.
        unsafe {
            let value = read_sysreg!(tcr_el1);
            write_sysreg!(tcr_el1, value | epd);
            core::arch::asm!("isb", options(nostack, preserves_flags));
        }
    }

    /// Разрешает обход таблиц по TTBR половины `T`.
    ///
    /// # Safety
    ///
    /// TTBR половины `T` содержит корень живых таблиц страниц. Другой код на этом CPU не пишет
    /// TCR_EL1 между чтением и записью, например IRQ замаскированы.
    pub unsafe fn enable_walks<T: TtbrSel>() {
        let epd = TCRBit::epd::<T>(false).encode();

        // SAFETY: см. `disable_walks`; обход пойдёт по корню, который контракт функции требует
        // от вызывающего.
        unsafe {
            let value = read_sysreg!(tcr_el1);
            write_sysreg!(tcr_el1, value & !epd);
            core::arch::asm!("isb", options(nostack, preserves_flags));
        }
    }
}

/// Конфигурация трансляции адресов.
pub struct AddressTranslationConfig<T: TtbrSel> {
    value: u64,
    _phantom: PhantomData<T>,
}

impl<T: TtbrSel> Copy for AddressTranslationConfig<T> {}
impl<T: TtbrSel> Clone for AddressTranslationConfig<T> {
    fn clone(&self) -> Self {
        *self
    }
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
    ///
    /// `asid_width` определяет глобальный бит `AS` (bit 36). Итоговое значение
    /// уходит под побитовое OR с конфигом второй половины, и ширина
    /// передаётся один раз - в любом из двух вызовов.
    pub const fn create(enable: bool, asid_width: AsidWidth) -> Self {
        Self::combine(&[
            TCRBit::size_48bit::<T>(),
            TCRBit::irgn_wb_wa::<T>(),
            TCRBit::orgn_wb_wa::<T>(),
            TCRBit::sh_inner::<T>(),
            TCRBit::tg_4k::<T>(),
            TCRBit::epd::<T>(enable),
            TCRBit::ips_40bit(),
            TCRBit::asid_size(asid_width),
        ])
    }
}
