//! Translation Lookaside Buffer
//! Кэш трансляций VA -> PA, чтобы MMU не ходил каждый раз по page tables
//!
use core::arch::asm;

use crate::memory::regs::common::{EL1, ExceptionLevel};

const PAGE_SHIFT: usize = 12;
const VA_OPERAND_MASK: u64 = 0x0000_0FFF_FFFF_FFFF;

pub(crate) struct TranslationLookasideBuffer<EL: ExceptionLevel> {
    _phantom: core::marker::PhantomData<EL>,
}

impl TranslationLookasideBuffer<EL1> {
    pub(crate) fn invalidate() {
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

    pub(crate) fn invalidate_global_inner_shareable() {
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

    pub(crate) fn invalidate_va_range_asid(va: usize, pages: usize, asid: u16) {
        // tlbi vae1is, Xt: Xt[63:48] = ASID, Xt[43:0] = VA[55:12]. Маска сохраняет биты
        // VA[55:48] адреса верхней половины, и по VA[55] выбирается TTBR1. Барьеры обрамляют
        // всю серию `tlbi`.
        let asid_bits = u64::from(asid) << 48;
        store_barrier();
        for page in 0..pages {
            let page_va = (va + (page << PAGE_SHIFT)) as u64;
            let xt = asid_bits | ((page_va >> PAGE_SHIFT) & VA_OPERAND_MASK);
            // SAFETY: `tlbi vae1is` принимает один регистровый операнд `Xt`;
            // инструкция не модифицирует память и регистры (preserves_flags).
            unsafe {
                asm!("tlbi vae1is, {xt}", xt = in(reg) xt, options(nostack, preserves_flags));
            }
        }
        completion_barrier();
    }

    pub(crate) fn invalidate_asid_inner_shareable(asid: u16) {
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

    pub(crate) fn invalidate_va_range_global(va: usize, pages: usize) {
        store_barrier();
        for page in 0..pages {
            // tlbi vaae1is, Xt: Xt[43:0] = VA[55:12].
            let xt = ((va + (page << PAGE_SHIFT)) as u64 >> PAGE_SHIFT) & VA_OPERAND_MASK;
            // SAFETY: `tlbi vaae1is` принимает один регистровый операнд `Xt`;
            // не модифицирует память.
            unsafe {
                asm!("tlbi vaae1is, {xt}", xt = in(reg) xt, options(nostack, preserves_flags));
            }
        }
        completion_barrier();
    }
}

fn store_barrier() {
    // Записи page tables становятся видимы обходчику до `tlbi`.
    // SAFETY: `dsb ishst` не имеет операндов и не модифицирует регистры.
    unsafe { asm!("dsb ishst", options(nostack, preserves_flags)) };
}

fn completion_barrier() {
    // `dsb ish` ждёт завершения `tlbi` во всех CPU домена, `isb` синхронизирует pipeline.
    // SAFETY: `dsb ish` и `isb` не имеют операндов и не модифицируют регистры.
    unsafe { asm!("dsb ish", "isb", options(nostack, preserves_flags)) };
}
