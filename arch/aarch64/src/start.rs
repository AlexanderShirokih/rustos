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

use crate::drivers::setup::{build_memory_layout, create_bump_allocator};
use crate::memory::global_allocator::GlobalKernelAllocator;
use crate::memory::layout::{MemoryLayout, MemoryRegion};
use crate::memory::manager::{MemoryManager, Prepared};
use aarch64_paging::preset::Mmio;
use alloc::boxed::Box;
use core::arch::{asm, naked_asm};
use core::hint::spin_loop;
use fdt::devicetree::DeviceTree;
use kernel::console::{GlobalWriter, set_early_stdout, set_stdout};
use kernel::driver::early::{EarlyDriverHandle, EarlyDriverRegistry};
use kernel::driver::{DriverRegistry, scanner};
use kernel::{debug, fatal, info};

/// Глобальный двухфазный аллокатор ядра
#[global_allocator]
static GLOBAL_ALLOCATOR: GlobalKernelAllocator = GlobalKernelAllocator::new();

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

    let bump_allocator = match create_bump_allocator(&memory_layout) {
        Ok(allocator) => allocator,
        Err(_) => return,
    };

    // Инициализируем ранний аллокатор
    GLOBAL_ALLOCATOR.init_bump_phase(bump_allocator);

    let mut early_registry = EarlyDriverRegistry::new();
    early_registry.scan_and_probe(&device_tree);

    let early_stdout = bind_early_stdout(&device_tree, &mut early_registry);
    info!("Early console set");

    for mmio_region in early_registry.mmio_region_requests() {
        memory_layout.add(MemoryRegion::new(
            "mmio",
            mmio_region.base,
            mmio_region.base + mmio_region.size,
            Mmio::flags(),
            true,
        ));
    }

    if setup_memory(memory_layout, early_stdout).is_err() {
        return;
    }

    let mut driver_registry = DriverRegistry::new();
    driver_registry.scan_and_probe(&device_tree);

    loop {
        spin_loop();
    }
}

fn setup_memory(
    memory_layout: MemoryLayout,
    early_stdout: Option<&'static GlobalWriter>,
) -> Result<(), ()> {
    // Создаем экземпляр менеджера памяти
    let memory_manager = MemoryManager::<Prepared>::create(memory_layout)
        .inspect_err(|err| fatal!("Memory setup failed: {:?}", err))
        .map_err(|_| ())?;
    debug!("Memory manager prepared!");

    // Делаем identity mapping b включаем MMU
    let memory_manager = memory_manager
        .enable()
        .inspect_err(|err| fatal!("Unable to enable MMU: {:?}", err))
        .map_err(|_| ())?;
    debug!("MMU enabled!");

    // Перемещаем ядро в higher half
    let _ = memory_manager.relocate();
    debug!("Kernel was relocated to higher half address space");

    // Переключаем глобальный аллокатор на heap-фазу
    // GLOBAL_ALLOCATOR.switch_to_heap(heap);

    if let Some(writer) = early_stdout {
        // После включения MMU можно включить normal mode и начать синхронизированный вывод.
        set_stdout(writer);
    }

    info!("Memory setup done!");

    Ok(())
}

fn bind_early_stdout(
    device_tree: &DeviceTree,
    registry: &mut EarlyDriverRegistry,
) -> Option<&'static GlobalWriter> {
    let early_console_node = scanner::find_console(&device_tree);

    let console_handle = early_console_node
        .and_then(|early_console_node| registry.take(&early_console_node.key()))
        .and_then(|early_console| match early_console {
            EarlyDriverHandle::Writer(console) => Some(console),
            EarlyDriverHandle::Opaque => None,
        });

    console_handle.map(|console| {
        let writer: &'static mut GlobalWriter = Box::leak(console);
        let writer: &'static GlobalWriter = writer;
        set_early_stdout(writer);
        writer
    })
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
