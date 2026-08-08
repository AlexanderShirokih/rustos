//! Точка входа Linux ARM64 boot protocol.

use core::{arch::naked_asm, hint::spin_loop};

use hal_common::boot::BootInfo;
use memory::physical_address::PhysicalAddress;

use crate::{
    boot::boot_early::{
        _bss_end, _bss_start, _stack_bottom, _stack_top, BOOT_STACK_FILL, boot_main,
    },
    consts::CNTKCTL_EL1_USER_COUNTER,
};

/// Точка входа ядра. Передаёт управление от загрузчика к `boot_main`.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub extern "C" fn _start() {
    naked_asm!(
        // Сохранение DTB (x0) в callee-saved регистре
        "mov    x19, x0",

        "mrs    x0, CurrentEL",
        "cmp    x0, #0x8",          // EL2?
        "b.ne   1f",                // если не EL2, идём сразу в EL1

        // Обработчик EL2

        // HCR_EL2: EL1 в AArch64
        "mov    x0, #(1 << 31)",
        "msr    hcr_el2, x0",

        // Отключение ловушек SIMD/FP
        "mov    x0, #0x33ff",
        "msr    cptr_el2, x0",

        // Разрешаем EL1 доступ к физическому таймеру/счётчику
        "mrs    x0, cnthctl_el2",
        "orr    x0, x0, #0b11",     // EL1PCTEN | EL1PCEN
        "msr    cnthctl_el2, x0",

        // Сбрасываем CNTVOFF_EL2: CNTVCT_EL0 тикает без смещения от физического счётчика.
        "msr    cntvoff_el2, xzr",

        // SPSR_EL2: возврат в EL1h, DAIF masked
        "mov    x0, #0x3c5",
        "msr    spsr_el2, x0",
        "adr    x0, 1f",
        "msr    elr_el2, x0",
        "eret",

        // EL1
        "1:",

        // Инициализация SP_EL1 (физический адрес через adrp)
        "msr    spsel, #1",
        "adrp   x1, {stack_top}",
        "add    x1, x1, #:lo12:{stack_top}",
        "mov    sp, x1",

        // Per-CPU блок ещё не установлен: TPIDR_EL1 после reset UNKNOWN,
        // а платформенный слой читает ноль как "блока нет".
        "msr    tpidr_el1, xzr",

        // Включение FP/SIMD
        "mrs    x0, cpacr_el1",
        "orr    x0, x0, #(0x3 << 20)",
        "msr    cpacr_el1, x0",
        "isb",

        // Открываем userspace счётчик времени и его частоту. Регистр пишется целиком: после
        // reset его значение неизвестно, а лишние биты открыли бы и управление таймером.
        "mov    x0, #{cntkctl}",
        "msr    cntkctl_el1, x0",
        "isb",

        // Сброс строк кэша над `.bss` и стеком до точки когерентности.
        // Записи ниже идут при выключенном MMU, то есть мимо кэша, и
        // оставленная загрузчиком грязная строка вытеснилась бы поверх них.
        "adrp   x0, {bss_start}",
        "add    x0, x0, #:lo12:{bss_start}",
        "adrp   x1, {stack_top}",
        "add    x1, x1, #:lo12:{stack_top}",
        "mrs    x2, ctr_el0",
        "ubfx   x2, x2, #16, #4",       // CTR_EL0.DminLine
        "mov    x3, #4",
        "lsl    x2, x3, x2",            // длина строки в байтах
        "sub    x3, x2, #1",
        "bic    x0, x0, x3",            // выравнивание начала наружу
        "12:",
        "dc     ivac, x0",
        "add    x0, x0, x2",
        "cmp    x0, x1",
        "b.lo   12b",
        "dsb    sy",

        // Заливка стека маркером. Идёт до первого использования стека:
        // post-MMU фаза измеряет по нему, сколько стека съела pre-MMU.
        "adrp   x0, {stack_bottom}",
        "add    x0, x0, #:lo12:{stack_bottom}",
        "adrp   x1, {stack_top}",
        "add    x1, x1, #:lo12:{stack_top}",
        "movz   x2, #{fill_0}",
        "movk   x2, #{fill_1}, lsl #16",
        "movk   x2, #{fill_2}, lsl #32",
        "movk   x2, #{fill_3}, lsl #48",
        "30:",
        "str    x2, [x0], #8",
        "cmp    x0, x1",
        "b.lo   30b",

        // Очистка BSS (физические адреса через adrp)
        "adrp   x0, {bss_start}",
        "add    x0, x0, #:lo12:{bss_start}",
        "adrp   x1, {bss_end}",
        "add    x1, x1, #:lo12:{bss_end}",
        "cmp    x0, x1",
        "b.ge   11f",
        "10:",
        "str    xzr, [x0], #8",
        "cmp    x0, x1",
        "b.lt   10b",
        "11:",

        // Восстанавливаем DTB в x0 и переходим в boot_main
        "mov    x0, x19",
        "b      {boot_main_entry}",

        // Если вернулись - уходим в WaitForEvent
        "msr    daifset, #0b0010",
        "20:",
        "wfe",
        "b      20b",

        bss_start       = sym _bss_start,
        bss_end         = sym _bss_end,
        stack_bottom    = sym _stack_bottom,
        stack_top       = sym _stack_top,
        fill_0          = const (BOOT_STACK_FILL & 0xFFFF) as u16,
        fill_1          = const ((BOOT_STACK_FILL >> 16) & 0xFFFF) as u16,
        fill_2          = const ((BOOT_STACK_FILL >> 32) & 0xFFFF) as u16,
        fill_3          = const ((BOOT_STACK_FILL >> 48) & 0xFFFF) as u16,
        cntkctl         = const CNTKCTL_EL1_USER_COUNTER,
        boot_main_entry = sym boot_main_entry,
    )
}

/// Обёртка для вызова из asm (не возвращает управление).
pub fn boot_main_entry(dtb_phys: usize) -> ! {
    let info = BootInfo::from_fdt(PhysicalAddress::new(dtb_phys));
    let _ = boot_main(&info);
    loop {
        spin_loop();
    }
}
