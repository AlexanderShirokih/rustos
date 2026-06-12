//! Translation Lookaside Buffer
//! Кэш трансляций VA -> PA, чтобы MMU не ходил каждый раз по page tables
//!
use core::arch::asm;

use crate::memory::regs::common::EL1;

/// TLB (Translation Lookaside Buffer).
pub struct TranslationLookasideBuffer<EL> {
    _phantom: core::marker::PhantomData<EL>,
}

impl TranslationLookasideBuffer<EL1> {
    /// Инвалидирует все записи TLB на текущем CPU.
    pub fn invalidate() {
        // SAFETY: `dsb`/`tlbi vmalle1`/`isb` - стандартная последовательность инвалидации TLB
        // на EL1, не имеет операндов, не модифицирует регистры (preserves_flags) и память.
        unsafe {
            asm!(
                "dsb ishst",    // Ожидание записи page tables
                "tlbi vmalle1", // Инвалидация TLB
                "dsb ish",      // Ожидание завершения tlbi
                "isb",          // Синхронизация pipeline
                options(nostack, preserves_flags),
            );
        }
    }

    /// Глобальная инвалидация TLB во всём inner-shareable домене (rollover ASID).
    pub fn invalidate_global_inner_shareable() {
        // SAFETY: `tlbi vmalle1is` инвалидирует все TLB во всех CPU inner-shareable
        // домена; не имеет операндов, не модифицирует регистры или память.
        unsafe {
            asm!(
                "dsb ishst",
                "tlbi vmalle1is",
                "dsb ish",
                "isb",
                options(nostack, preserves_flags),
            );
        }
    }

    /// Точечная инвалидация одной 4К-страницы для заданного `asid`,
    /// inner-shareable. `va` - выровнен на 4К.
    pub fn invalidate_va_asid(va: usize, asid: u16) {
        // tlbi vae1is, Xt: Xt[63:48] = ASID, Xt[43:0] = VA[55:12].
        let xt: u64 = (u64::from(asid) << 48) | (((va as u64) >> 12) & 0x0000_000F_FFFF_FFFF);
        // SAFETY: `tlbi vae1is` принимает один регистровый операнд `Xt`;
        // инструкция не модифицирует память и регистры (preserves_flags).
        unsafe {
            asm!(
                "dsb ishst",
                "tlbi vae1is, {xt}",
                "dsb ish",
                "isb",
                xt = in(reg) xt,
                options(nostack, preserves_flags),
            );
        }
    }

    /// Инвалидация всех записей конкретного ASID, inner-shareable.
    /// Используется для unmap'а L1 block (1 ГБ) в user-AS.
    pub fn invalidate_asid_inner_shareable(asid: u16) {
        // tlbi aside1is, Xt: Xt[63:48] = ASID.
        let xt: u64 = u64::from(asid) << 48;
        // SAFETY: `tlbi aside1is` принимает один регистровый операнд `Xt`;
        // не модифицирует память и регистры (preserves_flags).
        unsafe {
            asm!(
                "dsb ishst",
                "tlbi aside1is, {xt}",
                "dsb ish",
                "isb",
                xt = in(reg) xt,
                options(nostack, preserves_flags),
            );
        }
    }

    /// Точечная инвалидация одной 4К-страницы во всех ASID, inner-shareable.
    /// Используется для global (kernel) страниц, не привязанных к ASID.
    pub fn invalidate_va_global(va: usize) {
        // tlbi vaae1is, Xt: Xt[43:0] = VA[55:12].
        let xt: u64 = ((va as u64) >> 12) & 0x0000_000F_FFFF_FFFF;
        // SAFETY: `tlbi vaae1is` принимает один регистровый операнд `Xt`;
        // не модифицирует память.
        unsafe {
            asm!(
                "dsb ishst",
                "tlbi vaae1is, {xt}",
                "dsb ish",
                "isb",
                xt = in(reg) xt,
                options(nostack, preserves_flags),
            );
        }
    }
}
