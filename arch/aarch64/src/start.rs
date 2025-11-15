#![no_std]
#![no_main]

mod boot_header;
mod drivers;
mod fdt;
mod memory;

use crate::drivers::setup::build_memory_layout;
#[cfg(not(feature = "qemu_virt"))]
use crate::drivers::uart_dm::UartDm;
#[cfg(feature = "qemu_virt")]
use crate::drivers::uart_pl011::UartPl011;
use crate::fdt::DeviceTree;
use crate::memory::manager::MemoryManager;
use ::memory::physical::PageAlignedAddress;
use aarch64_paging::{EntryFlags, MemoryRegion};
use core::arch::asm;
use core::hint::spin_loop;
use core::ptr::addr_of;
use kernel_core::console::{BasicConsole, Console, set_console};
use kernel_core::io::writer::BlockingWriter;
use kernel_core::{fatal, info, printf};

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

unsafe fn early_main(dtb: usize) {
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

    info!(console, "Kernel started!");

    let mmio = MemoryRegion::new(
        "UART",
        PageAlignedAddress::from_usize_unchecked(0x09000000).as_usize(),
        PageAlignedAddress::from_usize_unchecked(0x09000000)
            .next_aligned()
            .as_usize(),
        EntryFlags::DEVICE,
    );

    let device_tree = match unsafe { DeviceTree::from_ptr(dtb) } {
        Some(d) => d,
        None => {
            fatal!(console, "Failed to parse device tree");
            return;
        }
    };

    set_console(console);

    // Создаем центральный менеджер памяти, который владеет всеми компонентами
    let memory_layout = match build_memory_layout(&device_tree, mmio) {
        Ok(m) => m,
        Err(error) => {
            fatal!(console, "Memory layout error {:?}", error.message);
            return;
        }
    };

    let mut memory_manager = match MemoryManager::new(memory_layout) {
        Ok(m) => m,
        Err(error) => {
            fatal!(console, "Memory setup error {:?}", error);
            return;
        }
    };

    info!(console, "Memory manager initialized!");

    match memory_manager.enable() {
        Ok(_) => printf!(console, "Memory setup done!\n"),
        Err(_) => {
            printf!(console, "FATAL: Memory enable error\n");
            return;
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
