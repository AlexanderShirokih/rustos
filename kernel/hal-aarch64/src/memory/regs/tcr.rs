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

pub(crate) trait TtbrSel {
    const TNSZ_SHIFT: u64;
    const EPD_SHIFT: u64;
    const IRGN_SHIFT: u64;
    const ORGN_SHIFT: u64;
    const SH_SHIFT: u64;
    const TG_SHIFT: u64;
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

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) struct TCRBit(u64);

impl TCRBit {
    pub(crate) const fn size_48bit<Sel: TtbrSel>() -> Self {
        Self(16 << Sel::TNSZ_SHIFT)
    }

    pub(crate) const fn irgn_wb_wa<Sel: TtbrSel>() -> Self {
        Self(0b01 << Sel::IRGN_SHIFT)
    }

    pub(crate) const fn orgn_wb_wa<Sel: TtbrSel>() -> Self {
        Self(0b01 << Sel::ORGN_SHIFT)
    }

    pub(crate) const fn sh_inner<Sel: TtbrSel>() -> Self {
        Self(0b11 << Sel::SH_SHIFT)
    }

    pub(crate) const fn tg_4k<Sel: TtbrSel>() -> Self {
        Self(Sel::TG_4K_VALUE << Sel::TG_SHIFT)
    }

    pub(crate) const fn epd<Sel: TtbrSel>(enable: bool) -> Self {
        let bit = if enable { 0b0 } else { 0b1 << Sel::EPD_SHIFT };

        Self(bit)
    }

    pub(crate) const fn ips_40bit() -> Self {
        Self(0b010 << 32)
    }

    pub(crate) const fn asid_size(width: AsidWidth) -> Self {
        match width {
            AsidWidth::Bits8 => Self(0),
            AsidWidth::Bits16 => Self(1 << 36),
        }
    }

    const fn encode(self) -> u64 {
        self.0
    }
}

pub(crate) struct TranslationControlRegister<EL: ExceptionLevel> {
    _phantom: PhantomData<EL>,
}

impl TranslationControlRegister<EL1> {
    pub(crate) fn set(
        lower: AddressTranslationConfig<LowerHalf>,
        higher: AddressTranslationConfig<HigherHalf>,
    ) {
        // SAFETY: Запись в TCR_EL1 допустима на EL1 до включения MMU.
        // Значение формируется из типобезопасных конфигураций LowerHalf/HigherHalf.
        unsafe { write_sysreg!(tcr_el1, lower.value | higher.value) };
    }

    // SAFETY: другой код на этом CPU не пишет TCR_EL1 между чтением и записью, например IRQ
    // замаскированы.
    pub(crate) unsafe fn disable_walks<T: TtbrSel>() {
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

    // SAFETY: TTBR половины `T` содержит корень живых таблиц страниц. Другой код на этом CPU не
    // пишет TCR_EL1 между чтением и записью, например IRQ замаскированы.
    pub(crate) unsafe fn enable_walks<T: TtbrSel>() {
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

pub(crate) struct AddressTranslationConfig<T: TtbrSel> {
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
    pub(crate) const fn combine(bits: &[TCRBit]) -> Self {
        Self {
            value: combine_bits!(bits),
            _phantom: PhantomData,
        }
    }

    pub(crate) const fn create(enable: bool, asid_width: AsidWidth) -> Self {
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
