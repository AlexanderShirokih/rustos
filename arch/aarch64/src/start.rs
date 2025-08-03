#![cfg(target_arch = "aarch64")]
#![no_std]
#![no_main]

pub mod memory;
pub mod uart;

use core::arch::asm;

// --- ARM64 Image header
core::arch::global_asm!(
    r#"
    .section .head, "a"
    .global _boot
_boot:
    b _start                            // code0: branch to _start
    .word 0                             // code1
    .quad 0                             // text_offset
    .quad _kernel_size                  // image_size
    .quad 0                             // flags
    .quad 0                             // res2
    .quad 0                             // res3
    .ascii "ARM\x64"                    // magic "ARMd"
    .word 0                             // res4
"#
);

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> () {
    unsafe {
        asm!(
        // Init SP
        "ldr    x0, =_stack_top",
        "mov    sp, x0",
        // Enable FP/SIMD
        "mrs    x0, cpacr_el1",
        "orr    x0, x0, #(0x3 << 20)",
        "msr    cpacr_el1, x0",
        "isb",
        // Jump to early_main
        "b      {early_main}",
        early_main = sym early_main,
        options(noreturn)
        )
    }
}

const UART_BASE: usize = 0x0C17_0000; // базу возьми из DT для lavender
const UART_TF: usize = 0x000C;
const UARTDM_TF: usize = 0x0070;
const UART_SR: usize = 0x0008;
const UART_SR_TX_READY: u32 = 1 << 2;

#[inline(always)]
unsafe fn mmio32(p: usize) -> *mut u32 {
    p as *mut u32
}
#[inline(always)]
unsafe fn mmio8(p: usize) -> *mut u8 {
    p as *mut u8
}
#[inline(always)]
unsafe fn barrier() {
    core::arch::asm!("dsb ish; isb", options(nostack, preserves_flags));
}

// Обычный MSM-UART: по 1 байту
pub unsafe fn uart_write_byte(c: u8) {
    while (core::ptr::read_volatile(mmio32(UART_BASE + UART_SR)) & (1 << 2)) == 0 {}
    core::ptr::write_volatile(mmio8(UART_BASE + UART_TF), c);
    barrier();
}

// UARTDM: можно паковать до 4 байт и писать словом в UARTDM_TF
pub unsafe fn uartdm_write_chunk(chunk: [u8; 4]) {
    while (core::ptr::read_volatile(mmio32(UART_BASE + UART_SR)) & (1 << 2)) == 0 {}
    let w = u32::from_le_bytes(chunk);
    core::ptr::write_volatile(mmio32(UART_BASE + UARTDM_TF), w);
    barrier();
}

pub unsafe fn uart_putc(c: u8) {
    // core::ptr::write_volatile((UART_BASE) as *mut u8, c)
    while (core::ptr::read_volatile(mmio32(UART_BASE + UART_SR)) & UART_SR_TX_READY) == 0 {}
    core::ptr::write_volatile(mmio32(UART_BASE + UARTDM_TF), c as u32);
    core::arch::asm!("dsb ish; isb", options(nostack, preserves_flags));
}

pub fn uart_puts(s: &str) {
    unsafe {
        for &b in s.as_bytes() {
            if b == b'\n' {
                uart_putc(b'\r')
            }
            uart_putc(b);
        }
    }
}

fn early_main() -> ! {
    unsafe {
        psci_system_reset();
        // uart_puts("Hello, world44444!\n");
        // loop {
        //     asm!("wfi");
        // }
    }
}

#[inline(always)]
fn psci_system_reset() -> ! {
    unsafe {
        core::arch::asm!(
        // x9 = CurrentEL >> 2 (1=EL1, 2=EL2, 3=EL3)
        "mrs x9, CurrentEL",
        "lsr x9, x9, #2",

        // --- Try SMC64 at EL1
        "cmp x9, #1",
        "b.ne 2f",
        "mov x1, xzr; mov x2, xzr; mov x3, xzr",
        // SYSTEM_RESET, SMCCC 64
        "mov x0, #0x0009",
        "movk x0, #0xC400, lsl #16",
        "smc #0",
        // If returned, try SMC32
        "mov x1, xzr; mov x2, xzr; mov x3, xzr",
        "mov x0, #0x0009",
        "movk x0, #0x8400, lsl #16",
        "smc #0",
        "b 4f",

        // --- Try HVC at EL2
        "2:",
        "cmp x9, #2",
        "b.ne 4f",
        "mov x1, xzr; mov x2, xzr; mov x3, xzr",
        // SYSTEM_RESET, SMCCC 64 via HVC
        "mov x0, #0x0009",
        "movk x0, #0xC400, lsl #16",
        "hvc #0",
        // If returned, try SMC32 via HVC
        "mov x1, xzr; mov x2, xzr; mov x3, xzr",
        "mov x0, #0x0009",
        "movk x0, #0x8400, lsl #16",
        "hvc #0",

        // --- If still here: hang (значит PSCI не сработал или код не дошёл)
        "4: wfi; b 4b",
        options(noreturn)
        )
    }
}
