#![no_std]
#![no_main]
extern crate alloc;

mod boot_header;
mod drivers;
mod memory;

use crate::drivers::setup::{build_memory_layout, create_bump_allocator};
use crate::drivers::uart_pl011::UartPl011;
use crate::memory::global_allocator::GlobalKernelAllocator;
use crate::memory::manager::MemoryManager;
use aarch64_paging::MemoryLayout;
use alloc::boxed::Box;
use core::arch::{asm, naked_asm};
use core::hint::spin_loop;
use fdt::devicetree::DeviceTree;
use kernel_core::console::{BasicConsole, Console, set_stdout, stdout};
use kernel_core::driver::early::{EarlyDriverHandle, EarlyDriverRegistry};
use kernel_core::driver::{DriverRegistry, scanner};
use kernel_core::io::writer::BlockingWriter;
use kernel_core::{fatal, info, printf};

/// Глобальный двухфазный аллокатор ядра
#[global_allocator]
static GLOBAL_ALLOCATOR: GlobalKernelAllocator = GlobalKernelAllocator::new();

unsafe extern "C" {
    static _stack_top: u8;
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub extern "C" fn _start() -> ! {
    naked_asm!(
        // Сохраняем DTB (x0) в callee-saved регистре
        "mov    x19, x0",

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

        // Восстанавливаем DTB в x0
        "mov    x0, x19",
        "b      {early_main}",
        stack_top = sym _stack_top,
        early_main = sym early_main,
    )
}

unsafe fn early_main(dtb: usize) {
    let device_tree = match DeviceTree::from_ptr(dtb) {
        Ok(tree) => tree,
        Err(_) => return,
    };

    let memory_layout = match build_memory_layout(&device_tree) {
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

    bind_stdout(&device_tree, &mut early_registry);

    info!(stdout(), "Kernel started!");

    setup_memory(memory_layout);

    let mut driver_registry = DriverRegistry::new();
    driver_registry.scan_and_probe(&device_tree);

    loop {
        spin_loop();
    }
}

fn setup_memory(memory_layout: MemoryLayout) {
    let memory_manager = match MemoryManager::new(memory_layout) {
        Ok(m) => m,
        Err(error) => {
            fatal!(stdout(), "Memory setup error {:?}", error);
            return;
        }
    };

    info!(stdout(), "Memory manager initialized!");

    match memory_manager.enable() {
        Ok(heap) => {
            // Переключаем глобальный аллокатор на heap-фазу
            GLOBAL_ALLOCATOR.switch_to_heap(heap);

            printf!(stdout(), "Memory setup done!\n")
        }
        Err(_) => {
            printf!(stdout(), "FATAL: Memory enable error\n");
            return;
        }
    };
}

fn bind_stdout(device_tree: &DeviceTree, registry: &mut EarlyDriverRegistry) {
    let early_console_node = scanner::find_console(&device_tree);

    let name = early_console_node.as_ref().unwrap().name();
    printf!(stdout(), "Early console: {:?}\n", name);

    let console_handle = early_console_node
        .and_then(|early_console_node| registry.take(early_console_node.name()))
        .and_then(|early_console| match early_console {
            EarlyDriverHandle::Console(console) => Some(console),
            EarlyDriverHandle::Opaque => None,
        });

    if let Some(console) = console_handle {
        set_stdout(Box::leak(console));
    }
}

// Поскольку мы находимся в no_std окружении, то нам нужен свой panic handler
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    unsafe {
        let uart = UartPl011::new(0x0900_0000);
        let writer = BlockingWriter::new(uart);
        let console = BasicConsole::new(writer);

        console.printf(format_args!("[PANIC] {}\n", info));

        loop {
            asm!("wfi", options(nomem, nostack));
        }
    }
}
