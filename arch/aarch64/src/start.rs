#![cfg(target_arch = "aarch64")]
#![no_std]
#![no_main]

#[cfg(not(feature = "qemu_virt"))]
use crate::drivers::uart_dm::UartDm;
#[cfg(feature = "qemu_virt")]
use crate::drivers::uart_pl011::UartPl011;

use crate::drivers::setup::MemoryLayoutBuilder;
use crate::fdt::DeviceTree;
use crate::memory::layout::MemoryLayout;
use crate::memory::manager::MemoryManager;
use core::arch::asm;
use core::arch::global_asm;
use core::hint::spin_loop;
use core::ptr::{addr_of, addr_of_mut};
use kernel_core::console::{BasicConsole, Console, set_console};
use kernel_core::io::writer::BlockingWriter;
use kernel_core::{info, printf};

mod drivers;
mod fdt;
mod memory;

// Заголовок формата Linux ARM64, для совместимости со стоковыми Android-загрузчиками
global_asm!(
    r#"
    .section .head, "ax"
    .balign 8
    .global _header_start
_header_start:
    b _start                            // code0: branch to _start
    .word 0                             // code1
    .quad 0                             // text_offset
    .quad _kernel_size                  // image_size
    .quad 0                             // flags
    .quad 0                             // res2
    .quad 0                             // res3
    .quad 0                             // res4
    .word 0x644D5241                    // magic "ARM\x64"
    .word 0                             // res5
"#
);

// Стек — 16 KiB
const STACK_SIZE: usize = 16 * 1024;

#[unsafe(link_section = ".bss.stack")]
static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let dtb: usize;

    unsafe {
        asm!(
        "mov {dtb}, x0",
        dtb = lateout(reg) dtb,
        );

        // верх стека = адрес сразу за массивом
        let top = (addr_of!(STACK).wrapping_add(1) as usize) & !0xF;

        asm!(
        // Используем SP_EL1
        "msr     spsel, #1",
        // Инициализация SP_EL1 (загрузчик должен передать управление в EL1)
        "mov    sp, {sp_top}",

        // Включаем FP/SIMD
        "mrs    x0, cpacr_el1",
        "orr    x0, x0, #(0x3 << 20)",
        "msr    cpacr_el1, x0",
        "isb",
        "mov    x0, {dtb}",
        "b      {early_main}",
        sp_top = in(reg) top,
        dtb = in(reg) dtb,
        early_main = sym early_main,
        options(noreturn)
        )
    }
}

unsafe extern "C" {
    static _kernel_start: u8;
    static _kernel_end: u8;
}

unsafe fn early_main(dtb: usize) {
    let device_tree = match unsafe { DeviceTree::from_ptr(dtb) } {
        Some(d) => d,
        None => return,
    };

    // Инициализируем bump allocator для ранних объектов
    let bump = memory::bump_allocator::bump_allocator();
    bump.init();

    // Размещаем UART и консоль в bump allocator
    #[cfg(feature = "qemu_virt")]
    let console: &'static BasicConsole<BlockingWriter<'static, UartPl011>> = unsafe {
        let uart_ptr = bump
            .alloc_ptr(UartPl011::new(0x09000000))
            .expect("Failed to allocate UART");

        let console_ptr = bump
            .alloc_ptr(BasicConsole::new(BlockingWriter::new(&*uart_ptr)))
            .expect("Failed to allocate console");

        &*console_ptr
    };

    #[cfg(not(feature = "qemu_virt"))]
    let console: &'static BasicConsole<BlockingWriter<'static, UartDm>> = unsafe {
        let uart_ptr = bump
            .alloc_ptr(UartDm::new(0x0C170000))
            .expect("Failed to allocate UART");

        let console_ptr = bump
            .alloc_ptr(BasicConsole::new(BlockingWriter::new(&*uart_ptr)))
            .expect("Failed to allocate console");

        &*console_ptr
    };

    printf!(console, "Hello, world!\n");

    info!(console, "Early console initialized! {}", "UART");

    let (kernel_start, kernel_end) = (
        addr_of!(_kernel_start) as usize,
        addr_of!(_kernel_end) as usize,
    );
    let (dtb_base, dtb_size) = (device_tree.base_address(), device_tree.size());

    let memory_layout_builder = MemoryLayoutBuilder {
        device_tree,
        kernel_start,
        kernel_end,
    };

    info!(
        console,
        "Kernel start address: {:#x} (size {} bytes)",
        kernel_start,
        kernel_end - kernel_start
    );
    info!(
        console,
        "DTB start address: {:#x} (size {} bytes)", dtb_base, dtb_size
    );

    set_console(console);

    // Создаем центральный менеджер памяти, который владеет всеми компонентами
    let memory_layout = MemoryLayout::from(memory_layout_builder);
    let mut memory_manager = match MemoryManager::new(memory_layout) {
        Ok(m) => m,
        Err(error) => {
            printf!(console, "FATAL: Memory setup error {:?}\n", error);
            return;
        }
    };

    info!(console, "Memory manager initialized!");

    unsafe {
        match memory_manager.enable() {
            Ok(_) => printf!(console, "Memory setup done!\n"),
            Err(_) => {
                printf!(console, "FATAL: Memory enable error\n");
                return;
            }
        }
    };

    loop {
        spin_loop();
    }
}

// Поскольку мы находимся в no_std окружении, то нам нужен свой panic handler
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe {
        loop {
            asm!("wfi", options(nomem, nostack));
        }
    }
}
