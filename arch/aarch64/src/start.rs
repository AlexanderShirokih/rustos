#![no_std]
#![no_main]
extern crate alloc;

#[cfg(test)]
extern crate std;

mod boot_header;

mod drivers;
mod exceptions;
mod memory;
mod system;

use crate::memory::layout::{MemoryLayout, MemoryRegion};
use crate::memory::memory_setup::{Early, Installed, MemorySetup};
use alloc::boxed::Box;
use core::arch::{asm, naked_asm};
use core::hint::spin_loop;
use fdt::devicetree::DeviceTree;
use kernel::driver::early::EarlyDriverRegistry;
use kernel::driver::scanner;
use kernel::kmain::kmain;
use klog::{debug, fatal, info, set_early_stdout};
use memory::setup::build_memory_layout;

unsafe extern "C" {
    static _stack_top: u8;
    static _kernel_start: u8;
    static _bss_start: u8;
    static _bss_end: u8;
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub extern "C" fn _start() -> ! {
    naked_asm!(
        // Сохраняем DTB (x0) в callee-saved регистре
        "mov    x19, x0",

        // Проверяем EL
        "mrs    x0, CurrentEL",
        "cmp    x0, #0x8",          // Мы получили управление с EL2 (гипервизор)?
        "b.ne   .boot_el1",

        // Обработчик EL2

        // HCR_EL2: EL1 будет работать в 64-битном режиме
        "mov    x0, #(1 << 31)",
        "msr    hcr_el2, x0",

        // Отключаем ловушки для SIMD/FP
        "mov    x0, #0x33ff",
        "msr    cptr_el2, x0",

        // SPSR_EL2: возврат в EL1h с замаскированными DAIF
        "mov    x0, #0x3c5",
        "msr    spsr_el2, x0",
        "adr    x0, .boot_el1",
        "msr    elr_el2, x0",
        "eret",

        ".boot_el1:",
        // Инициализация SP_EL1
        "msr    spsel, #1",
        "adrp   x1, {stack_top}",
        "add    x1, x1, #:lo12:{stack_top}",
        "mov    sp, x1",

        // Включаем FP/SIMD
        "mrs    x0, cpacr_el1",
        "orr    x0, x0, #(0x3 << 20)",
        "msr    cpacr_el1, x0",
        "isb",

        // Очистка BSS: x0 = &_bss_start, x1 = &_bss_end
        "adrp   x0, {bss_start}",
        "add    x0, x0, #:lo12:{bss_start}",
        "adrp   x1, {bss_end}",
        "add    x1, x1, #:lo12:{bss_end}",
        "cmp    x0, x1",
        "b.ge   2f",
        "1:",
        "str    xzr, [x0], #8",
        "cmp    x0, x1",
        "b.lt   1b",
        "2:",

        // Восстанавливаем DTB в x0
        "mov    x0, x19",

        // "mov x20, xzr",
        // "2: wfe",
        // "cbz x20, 2b",

        "b      {early_main}",

        // Если мы вернулись из early_main, то выключаем прерывания и зацикливаемся в WaitForEvent
        "msr    daifset, #0b0010",
        "1:     wfe",
        "b      1b",
        bss_start = sym _bss_start,
        bss_end = sym _bss_end,
        stack_top = sym _stack_top,
        early_main = sym early_main,
    )
}

fn early_main(dtb: usize) {
    exceptions::init();

    let device_tree = match DeviceTree::from_ptr(dtb) {
        Ok(tree) => tree,
        Err(_) => return,
    };

    let mut memory_layout = match build_memory_layout(&device_tree) {
        Ok(m) => m,
        Err(_) => return,
    };

    // Создаём Early memory setup и устанавливаем bump allocator
    let early_setup = match MemorySetup::<Early>::create(&memory_layout) {
        Ok(setup) => setup.install(),
        Err(_) => return,
    };

    let mut early_registry = EarlyDriverRegistry::new();
    early_registry.scan_and_probe(&device_tree);

    // Утекаем реестр в статическую память — драйверы живут до конца работы ядра,
    // потому что writer из output() сохраняется в статическую переменную STDOUT
    let early_registry: &'static EarlyDriverRegistry = Box::leak(Box::new(early_registry));

    bind_early_stdout(&device_tree, early_registry);
    info!("Early console set");

    for mmio_region in early_registry.mmio_region_requests() {
        memory_layout.add(MemoryRegion::mmio(mmio_region.base, mmio_region.size));
    }

    info!("Memory layout:");
    for region in memory_layout.iter() {
        info!(
            "- {:?}, from {:#x} to {:#x}",
            region.tag,
            region.start.as_usize(),
            region.end.as_usize()
        );
    }

    if setup_memory(memory_layout, early_setup).is_err() {
        return;
    }

    debug!("Early stage setup done!");

    // Основная платформозависимая настройка завершена. Переходим к общей точке входа
    kmain();

    loop {
        spin_loop();
    }
}

fn setup_memory(layout: MemoryLayout, installed: MemorySetup<Installed>) -> Result<(), ()> {
    // Переходим из Installed в Prepared фазу, резервируя bump region
    let memory_setup = installed
        .prepare(&layout)
        .inspect_err(|err| fatal!("Memory setup failed: {:?}", err))
        .map_err(|_| ())?;
    debug!("Memory setup prepared!");

    // Маппим higher half и включаем MMU
    let memory_setup = memory_setup
        .enable()
        .inspect_err(|err| fatal!("Unable to enable MMU: {:?}", err))
        .map_err(|_| ())?;

    // Прыжок в higher half — после этого PC указывает на HIGHER_HALF_BASE + PA
    unsafe { jump_to_higher_half() };

    info!("MMU enabled, running in higher half!");

    memory_setup
        .install()
        .inspect_err(|_| fatal!("Unable to set heap allocator"))?;

    info!("Global allocator switched to heap phase");

    Ok(())
}

/// Прыжок с identity адреса на higher half адрес.
/// После этого PC указывает на HIGHER_HALF_BASE + текущий PA.
#[unsafe(naked)]
unsafe extern "C" fn jump_to_higher_half() {
    use crate::memory::setup::HIGHER_HALF_BASE;

    // Разбиваем HIGHER_HALF_BASE на 16-битные части для movz/movk
    const HALF_47_32: u64 = ((HIGHER_HALF_BASE as u64) >> 32) & 0xFFFF;
    const HALF_63_48: u64 = ((HIGHER_HALF_BASE as u64) >> 48) & 0xFFFF;

    naked_asm!(
        "adr x0, 1f",                      // x0 = адрес метки 1 (identity)
        "movz x1, #{half_32}, lsl #32",    // x1[47:32]
        "movk x1, #{half_48}, lsl #48",    // x1[63:48]
        "add x0, x0, x1",                  // x0 = identity + HIGHER_HALF_BASE
        "br x0",                           // Прыжок на higher half адрес
        "1:",                              // Метка — сюда придём уже по higher half адресу
        "ret",
        half_32 = const HALF_47_32,
        half_48 = const HALF_63_48,
    )
}

fn bind_early_stdout(device_tree: &DeviceTree, registry: &'static EarlyDriverRegistry) {
    let early_console_node = scanner::find_console(&device_tree);

    let writer = early_console_node
        .map(|node| node.key())
        .and_then(|key| registry.get(&key))
        .and_then(|driver| driver.output());

    if let Some(writer) = writer {
        set_early_stdout(writer);
    }
}

// Поскольку мы находимся в no_std окружении, то нам нужен свой panic handler
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    unsafe {
        fatal!("Kernel panic: {}", info);

        loop {
            asm!("wfi", options(nomem, nostack));
        }
    }
}
