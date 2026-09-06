//! Кэш данных: обслуживание диапазонов по адресу.

use core::arch::asm;

use crate::memory::regs::common::{EL1, ExceptionLevel};

/// Кэш данных.
pub struct DataCache<EL: ExceptionLevel> {
    _phantom: core::marker::PhantomData<EL>,
}

impl DataCache<EL1> {
    /// Инвалидирует `[start, end)` до точки когерентности, отбрасывая и
    /// грязные строки. Границы округляются наружу до строки кэша.
    ///
    /// Вызывается при выключенном MMU, где адрес операнда `dc` равен
    /// физическому. Диапазон обязан принадлежать ядру. Содержимое грязных строк в этом
    /// диапазоне теряется.
    pub fn invalidate_range_to_coherency(start: usize, end: usize) {
        if start >= end {
            return;
        }

        let line = Self::line_size();
        let mut address = start & !(line - 1);

        while address < end {
            // SAFETY: `dc ivac` принимает один регистровый операнд с адресом,
            // не модифицирует регистры и разрешена на EL1.
            unsafe { asm!("dc ivac, {}", in(reg) address, options(nostack, preserves_flags)) };
            address += line;
        }

        // SAFETY: `dsb sy` - общесистемный барьер без операндов, дожидается
        // завершения обслуживания кэша.
        unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
    }

    /// Вытесняет `[start, end)` до точки когерентности, сохраняя содержимое.
    ///
    /// Делает записи видимыми тому, кто читает мимо кэша: стартующему CPU до
    /// включения MMU. Границы округляются наружу до строки кэша.
    pub fn clean_range_to_coherency(start: usize, end: usize) {
        if start >= end {
            return;
        }

        let line = Self::line_size();
        let mut address = start & !(line - 1);

        while address < end {
            // SAFETY: `dc cvac` принимает один регистровый операнд с адресом,
            // не модифицирует регистры и разрешена на EL1.
            unsafe { asm!("dc cvac, {}", in(reg) address, options(nostack, preserves_flags)) };
            address += line;
        }

        // SAFETY: `dsb sy` - общесистемный барьер без операндов, дожидается
        // завершения обслуживания кэша.
        unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
    }

    fn line_size() -> usize {
        let ctr: u64;
        // SAFETY: CTR_EL0 читается на EL1 без побочных эффектов.
        unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };

        // CTR_EL0.DminLine [19:16] - двоичный логарифм числа 4-байтовых слов
        // в строке.
        4usize << ((ctr >> 16) & 0xF)
    }
}
