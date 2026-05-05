//! Pre-MMU фаза загрузки: от точки входа до включения MMU.
//!
//! Ограничения этой фазы (higher-half VMA, MMU выключен):
//! - Абсолютные указатели в данных (vtable, fat ptr data) хранят VMA -> недоступны.
//! - Нельзя использовать `dyn Trait` dispatch, `klog`.

use core::{arch::naked_asm, hint::spin_loop};

use fdt::devicetree::DeviceTree;
use memory::virtual_address::{PageAlignedVirtualAddress, VirtualAddress};

use super::boot_primary::primary_main;
use crate::{
    HIGHER_HALF_BASE,
    memory::{
        memory_setup::{Early, Enabled, MemorySetup},
        setup::build_memory_layout,
    },
};

unsafe extern "C" {
    static _stack_top: u8;
    static _bss_start: u8;
    static _bss_end: u8;
}

/// Точка входа ядра. Передаёт управление от загрузчика к `boot_main`.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub extern "C" fn _start() {
    naked_asm!(
        // Сохранение DTB (x0) в callee-saved регистре
        "mov    x19, x0",

        // Проверяем EL
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
        "b      {boot_main}",

        // Если вернулись - уходим в WaitForEvent
        "msr    daifset, #0b0010",
        "20:",
        "wfe",
        "b      20b",

        bss_start = sym _bss_start,
        bss_end   = sym _bss_end,
        stack_top = sym _stack_top,
        boot_main = sym boot_main_entry,
    )
}

/// Обёртка для вызова из asm (не возвращает управление).
fn boot_main_entry(dtb_phys: usize) -> ! {
    let _ = boot_main(dtb_phys);
    loop {
        spin_loop();
    }
}

/// Pre-MMU фаза: DTB -> память -> bump allocator -> page tables -> MMU -> jump.
fn boot_main(dtb_phys: usize) -> Result<(), ()> {
    let device_tree = DeviceTree::from_ptr(dtb_phys).map_err(|_| ())?;

    // Строим раскладку памяти из DTB + символов линкера (физические адреса через adrp)
    let memory_layout = build_memory_layout(&device_tree).map_err(|_| ())?;

    // Создаём MemorySetup и устанавливаем bump-аллокатор
    let early_setup = MemorySetup::<Early>::create(memory_layout).map_err(|_| ())?;
    let installed = early_setup.install();

    // Подготавливаем таблицы страниц (freeze bump, create frame allocator, map RAM)
    let prepared = installed.prepare().map_err(|_| ())?;

    // Маппим все регионы и включаем MMU
    let higher_half_base =
        PageAlignedVirtualAddress::new_unchecked(VirtualAddress::new(HIGHER_HALF_BASE));
    let enabled = prepared.enable(higher_half_base).map_err(|_| ())?;

    let Enabled {
        roots,
        frame_allocator,
        ..
    } = enabled.state;
    let higher_root_pa = roots.higher_pa.as_usize();
    let frame_allocator_phys = frame_allocator as *const _ as usize;

    // Прыжок в higher half - управление передаётся в primary_main и не возвращается
    // SAFETY: MMU включён, TTBR1 содержит корректный маппинг higher-half.
    unsafe { jump_to_higher_half(dtb_phys, higher_root_pa, frame_allocator_phys) };

    loop {
        spin_loop();
    }
}

/// Прыжок с identity-адреса на higher-half адрес, переключение стека, вызов primary_main.
/// После прыжка управление передаётся в `primary_main` и не возвращается.
///
/// # Safety
///
/// MMU должен быть включён. TTBR1 должен содержать корректный маппинг higher-half.
/// TTBR0 должен покрывать текущий PC (identity mapping ещё нужен для инструкции `br x0`).
#[unsafe(naked)]
unsafe extern "C" fn jump_to_higher_half(
    _dtb_phys: usize,
    _higher_root_pa: usize,
    _frame_allocator_phys: usize,
) {
    const HALF_47_32: u64 = ((HIGHER_HALF_BASE as u64) >> 32) & 0xFFFF;
    const HALF_63_48: u64 = ((HIGHER_HALF_BASE as u64) >> 48) & 0xFFFF;

    naked_asm!(
        // extern "C" ABI (AArch64): аргументы в x0-x2:
        //   x0 = dtb_phys
        //   x1 = higher_root_pa
        //   x2 = frame_allocator_phys
        "mov x19, x0",
        "mov x20, x1",
        "mov x21, x2",

        // Вычисляем виртуальный адрес метки 1f и прыгаем на него
        "adr x0, 1f",
        "movz x1, #{half_32}, lsl #32",
        "movk x1, #{half_48}, lsl #48",
        "add x0, x0, x1",
        "br x0",                        // PC теперь виртуальный (TTBR1)

        "1:",
        // Переключаем стек на виртуальный адрес.
        // ldr загружает значение VMA из literal pool (замапленного через TTBR1).
        "ldr x1, ={stack_top}",
        "mov sp, x1",

        // Передаём параметры в primary_main:
        // x0=dtb_phys, x1=higher_root_pa, x2=frame_allocator_phys
        "mov x0, x19",
        "mov x1, x20",
        "mov x2, x21",
        "b {primary_main}",

        // Сюда не должны добраться
        "2:",
        "wfi",
        "b 2b",

        half_32 = const HALF_47_32,
        half_48 = const HALF_63_48,
        stack_top = sym _stack_top,
        primary_main = sym primary_main,
    )
}
