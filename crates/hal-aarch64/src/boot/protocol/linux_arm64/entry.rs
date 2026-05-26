//! Точка входа Linux ARM64 boot protocol.

use core::{arch::naked_asm, hint::spin_loop};

use hal_common::boot::BootInfo;
use memory::physical_address::PhysicalAddress;

use crate::boot::boot_early::{_bss_end, _bss_start, _stack_top, boot_main};

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

        // Сбрасываем CNTVOFF_EL2: гарантируем, что CNTVCT_EL0 тикает без смещения относительно физического.
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

        // Включение FP/SIMD
        "mrs    x0, cpacr_el1",
        "orr    x0, x0, #(0x3 << 20)",
        "msr    cpacr_el1, x0",
        "isb",

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
        stack_top       = sym _stack_top,
        boot_main_entry = sym boot_main_entry,
    )
}

/// Обёртка для вызова из asm (не возвращает управление).
pub fn boot_main_entry(dtb_phys: usize) -> ! {
    let info = BootInfo::new_fdt(PhysicalAddress::new(dtb_phys));
    let _ = boot_main(&info);
    loop {
        spin_loop();
    }
}
