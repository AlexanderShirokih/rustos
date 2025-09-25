#![cfg(target_arch = "aarch64")]
#![no_std]
#![no_main]

pub mod memory;
pub mod uart_mmio;

use crate::uart_mmio::UartMmio;
use arch_common::start::main;
use core::arch::asm;
use core::hint;
use kernel_core::console;
use kernel_core::console::{get_console, BasicConsole, Console};
use kernel_core::device::device::Device;
use kernel_core::writer::{BlockingWriter, Writer};
use spin::Once;

// Заголовок формата Linux ARM64, для совместимости со стоковыми Android-загрузчиками
core::arch::global_asm!(
    r#"
    .section .head, "ax"
    .global _header_start
_header_start:
    b _start                            // code0: branch to _start
    .word 0                             // code1
    .quad 0x80000                       // text_offset
    .quad _kernel_size                  // image_size
    .quad 0                             // flags
    .quad 0                             // res2
    .quad 0                             // res3
    .ascii "ARM\x64"                    // magic "ARMd"
    .word 0                             // res4
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
    static __bss_start: u8;
    static __bss_end: u8;
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

        // определить текущий EL
        "mrs    x1, CurrentEL",
        "lsr    x1, x1, #2",       // 1=EL1, 2=EL2, 3=EL3

        // EL2 -> EL1
        "cmp    x1, #2",
        "b.ne   1f",
        "msr    sp_el1, {sp_top}",
        // SPSR_EL2: DAIF=1111, M=EL1h (0101)
        "mov    x2, #(0b0101 | (1<<9) | (1<<8) | (1<<7) | (1<<6))",
        "msr    spsr_el2, x2",
        "adr    x2, 2f",
        "msr    elr_el2, x2",
        "mov    x2, #(1 << 31)",   // HCR_EL2.RW = 1 (EL1 = AArch64)
        "msr    hcr_el2, x2",
        "eret",

        // EL3 (вдруг) -> EL2 -> EL1
        "1:",
        "cmp    x1, #3",
        "b.ne   2f",
        "mov    x2, #(1<<8)",      // SCR_EL3.HCE = 1 (разрешить EL2)
        "msr    scr_el3, x2",
        // SPSR_EL3: DAIF=1111, M=EL2h (1001)
        "mov    x2, #(0b1001 | (1<<9) | (1<<8) | (1<<7) | (1<<6))",
        "adr    x3, 3f",
        "msr    elr_el3, x3",
        "eret",

        "3:",
        "msr    sp_el1, {sp_top}",
        "mov    x2, #(0b0101 | (1<<9) | (1<<8) | (1<<7) | (1<<6))",
        "msr    spsr_el2, x2",
        "adr    x2, 2f",
        "msr    elr_el2, x2",
        "eret",

        // Инициализация SP EL1
        "2:",
        "mov    sp, {sp_top}",
        // Включаем FP/SIMD
        "mrs    x0, cpacr_el1",
        "orr    x0, x0, #(0x3 << 20)",
        "msr    cpacr_el1, x0",
        "isb",

        // Jump to early_main
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

fn early_main(_dtb: usize) -> ! {
    // Очищаем область глобальных/статических переменных
    zero_bss();

    // Настраиваем ранний обработчик прерываний
    setup_vectors_el1();

    // Настраиваем ранний UART
    let uart0: &'static UartMmio = UART0.call_once(|| UartMmio::new(UART_BASE));
    uart0.init();

    let w = BlockingWriter::new(uart0);
    let cons_ref = EARLY_CONSOLE.call_once(|| BasicConsole::new(w));

    cons_ref.print("Hello, world 3!\n");
    cons_ref.print("Hello, world 4!\n");

    console::set_console(cons_ref as &'static dyn Console);

    let same = core::ptr::eq::<dyn Console>(
        console::get_console(),
        cons_ref as &dyn Console,
    );

    if same {
        cons_ref.print("Same!\n");
    } else {
        cons_ref.print("Not the same!\n");

    }
    // 1) Прямо через глобалку — должен работать
    kernel_core::console::get_console().print("ok via get_console\n");

    // 2) UFCS по трейту — тоже должен работать
    <dyn kernel_core::console::Console>::print(
        kernel_core::console::get_console(),
        "ok via UFCS\n",
    );

    cons_ref.print("Hello, world 5!\n");

    console::print("[P2] second print completed\n");
    console::info("[I1] info");
    console::print("[P3] ok\n");
    // main();

    loop {
        hint::spin_loop();
    }
}

#[inline(always)]
fn zero_bss() -> () {
    unsafe {
        let start = &__bss_start as *const u8 as *mut u8;
        let end = &__bss_end as *const u8 as usize;
        let size = end - (start as usize);

        core::ptr::write_bytes(start, 0, size);

        asm!("dsb sy; isb", options(nostack, preserves_flags));
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
