#![cfg(target_arch = "aarch64")]
#![no_std]
#![no_main]

pub mod memory;
pub mod uart_mmio;
pub mod fdt;
pub mod framebuffer;

use crate::uart_mmio::UartMmio;
use arch_common::start::main;
use core::arch::asm;
use core::hint;
use kernel_core::console;
use kernel_core::console::{BasicConsole, Console};
use kernel_core::device::device::Device;
use kernel_core::writer::BlockingWriter;
use spin::Once;
use util::string::{usize_to_hex_str, usize_to_str};

// Заголовок формата Linux ARM64, для совместимости со стоковыми Android-загрузчиками
core::arch::global_asm!(
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
    .word 0x644D5241                    // magic "ARM\x64" as string
    .word 0                             // res5
"#
);

// Базовая настройка векторов исключений для обнаружения проблем на раннем этапе
core::arch::global_asm!(
    r#"
    .section .text.vectors, "ax"
    .align 11                           // 2 KiB выравнивание (требуется для VBAR_EL1)
    .global __el1_vectors
__el1_vectors:
    // 0..7: все источники -> один обработчик
    b __el1_trap
    .balign 128
    b __el1_trap
    .balign 128
    b __el1_trap
    .balign 128
    b __el1_trap
    .balign 128
    b __el1_trap
    .balign 128
    b __el1_trap
    .balign 128
    b __el1_trap
    .balign 128
    b __el1_trap
    .balign 128

__el1_trap:
    bl __el1_fault
1:  wfi
    b 1b
"#
);

// Стек — 16 KiB
#[unsafe(link_section = ".bss.stack")]
static mut STACK: [u8; 16 * 1024] = [0; 16 * 1024];

unsafe extern "C" {
    static __el1_vectors: u8;
}

#[allow(static_mut_refs)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
pub extern "C" fn _start() -> ! {
    unsafe {
        // top = адрес конца массива STACK (16-байтовое выравнивание соблюдается)
        let top = (&STACK as *const _ as usize) + size_of_val(&STACK);

        asm!(
        // Cохранить DTB из x0
        "mov    x19, x0",
        // Инициализация SP EL1 (вход гарантированно в EL1)
        "mov    sp, {sp_top}",
        // Включаем FP/SIMD
        "mrs    x0, cpacr_el1",
        "orr    x0, x0, #(0x3 << 20)",
        "msr    cpacr_el1, x0",
        "isb",

        // Переход в early_main
        "mov    x0, x19",
        "b      {early_main}",
        early_main = sym early_main,
        sp_top = in(reg) top,
        options(noreturn)
        )
    }
}

static UART0: Once<UartMmio> = Once::new();
static EARLY_CONSOLE: Once<BasicConsole<BlockingWriter<'static, UartMmio>>> = Once::new();

fn early_main(dtb: usize) -> ! {
    // Настраиваем ранний обработчик прерываний
    setup_vectors_el1();

    // Настраиваем ранний UART
    let uart0: &'static UartMmio = UART0.call_once(|| UartMmio::new(UART_BASE));
    uart0.init();

    let w = BlockingWriter::new(uart0);
    let cons_ref = EARLY_CONSOLE.call_once(|| BasicConsole::new(w));

    cons_ref.print("Hello, world!\n");
    console::set_console(cons_ref);

    

    // Попытка найти и отрисовать framebuffer из DTB
    unsafe {
        if let Some(info) = fdt::find_framebuffer(dtb) {
            use crate::framebuffer::{Framebuffer, PixelFormat as FbPixelFormat, flush_framebuffer};
            let fmt = match info.format {
                fdt::PixelFormat::Argb8888 => FbPixelFormat::Argb8888,
                fdt::PixelFormat::Xrgb8888 => FbPixelFormat::Xrgb8888,
                fdt::PixelFormat::Rgb565 => FbPixelFormat::Rgb565,
                _ => FbPixelFormat::Xrgb8888,
            };
            let mut fb = Framebuffer {
                ptr: info.paddr as *mut u8,
                width: info.width as usize,
                height: info.height as usize,
                stride_bytes: info.stride as usize,
                bpp: info.bpp as usize,
                format: fmt,
            };
            console::info("Framebuffer detected via DTB");
            // Сообщим параметры в UART
            console::print("FB paddr=");
            console::print(usize_to_hex_str(info.paddr));
            console::print(" size=");
            console::print(usize_to_str(info.size));
            console::print(" width=");
            console::print(usize_to_str(info.width as usize));
            console::print(" height=");
            console::print(usize_to_str(info.height as usize));
            console::print(" stride=");
            console::print(usize_to_str(info.stride as usize));
            console::print(" bpp=");
            console::print(usize_to_str(info.bpp as usize));
            console::print("\r\n");
            // Заливка фона и прямоугольника
            fb.clear(0xFF1E1E1E);
            let rect_w = (fb.width / 4).max(50);
            let rect_h = (fb.height / 6).max(30);
            fb.fill_rect(20, 20, rect_w, rect_h, 0xFFFF5500);
            // Полоса снизу
            fb.fill_rect(0, fb.height.saturating_sub(16), fb.width, 16, 0xFF00AAFF);
            // Сброс кэша, если включен
            flush_framebuffer(fb.ptr, fb.stride_bytes * fb.height);
        } else {
            console::warn("No framebuffer node found in DTB");
        }
    }

    main();

    loop {
        hint::spin_loop();
    }
}

#[inline(always)]
fn setup_vectors_el1() {
    unsafe {
        let vec_base = &__el1_vectors as *const _ as u64;

        asm!(
            "msr VBAR_EL1, {base}",
            "dsb sy",
            "isb",
            base = in(reg) vec_base,
            options(nostack, preserves_flags)
        );
    }
}

#[unsafe(no_mangle)]
extern "C" fn __el1_fault() -> ! {
    unsafe {
        console::fatal("\nFAULT\n");

        asm!(
            "msr daifset, #0b1111",
            "dsb sy",
            "isb",
            options(nostack, preserves_flags)
        );
        loop {
            asm!("wfi", options(nomem, nostack));
        }
    }
}

#[panic_handler]
unsafe fn panic(_info: &core::panic::PanicInfo) -> ! {
    console::fatal("\nPANIC\n");

    unsafe {
        loop {
            asm!("wfi", options(nomem, nostack));
        }
    }
}

const UART_BASE: usize = 0x0C170000; // BLSP1 UART2 (sdm660)
