#![cfg(target_arch = "aarch64")]
#![no_std]
#![no_main]

use crate::drivers::uart_dm::UartDm;
use crate::drivers::uart_pl011::UartPl011;

use crate::fdt::DeviceTree;
use core::arch::asm;
use core::arch::global_asm;
use core::hint::spin_loop;
use core::ptr::addr_of;
use drivers::framebuffer;
use kernel_core::io::writer::BlockingWriter;

mod drivers;
mod fdt;

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
    unsafe {
        // верх стека = адрес сразу за массивом
        let top = (addr_of!(STACK).wrapping_add(1) as usize) & !0xF;

        asm!(
            // Cохранить DTB из x0
            "mov    x19, x0",
            // Используем SP_EL1
            "msr     spsel, #1",
            // Инициализация SP_EL1 (загрузчик должен передать управление в EL1)
            "mov    sp, {sp_top}",

            // Включаем FP/SIMD
            "mrs    x0, cpacr_el1",
            "orr    x0, x0, #(0x3 << 20)",
            "msr    cpacr_el1, x0",
            "isb",

            "mov    x0, x19",
            "b      {early_main}",
            early_main = sym early_main,
            sp_top = in(reg) top,
            options(noreturn)
        )
    }
}

fn early_main(dtb: usize) {
    let device_tree = match unsafe { DeviceTree::from_ptr(dtb) } {
        Some(d) => d,
        None => return,
    };

    #[cfg(feature = "qemu_virt")]
    let uart = UartPl011::new(0x09000000);
    #[cfg(not(feature = "qemu_virt"))]
    let uart = UartDm::new(0x0C170000);

    let console = BlockingWriter::new(&uart);

    console.print("Hello, world\n");

    unsafe {
        if let Some(info) = framebuffer::find_in_dtb(&device_tree) {
            use drivers::framebuffer::{Framebuffer, flush_framebuffer};

            let mut fb = Framebuffer {
                ptr: info.paddr as *mut u8,
                width: info.width as usize,
                height: info.height as usize,
                stride_bytes: info.stride as usize,
                bpp: info.bpp as usize,
                format: info.format,
            };

            // Заливка фона и прямоугольника
            fb.clear(0xFF1E1E1E);
            let rect_w = (fb.width / 4).max(50);
            let rect_h = (fb.height / 6).max(30);
            fb.fill_rect(20, 20, rect_w, rect_h, 0xFFFF5500);

            // Сброс кэша, если включен
            flush_framebuffer(fb.ptr, fb.stride_bytes * fb.height);
        } else {
            console.print("No framebuffer node found in DTB\n");
        }
    }

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
