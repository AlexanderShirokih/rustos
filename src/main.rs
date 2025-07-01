#![no_std]
#![no_main]

use core::arch::asm;
use core::hint;
use core::panic::PanicInfo;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    unsafe {
        asm!(
            // Enable FP/SIMD
            "mrs    x0, cpacr_el1",
            "orr    x0, x0, #(0x3 << 20)",
            "msr    cpacr_el1, x0",
            "isb",
            // Init SP
            "ldr    x0, =_stack_top",
            "mov    sp, x0",
            // Jump into Rust entry
            "b      main",
            options(noreturn)
        );
    }
}

const UART0_DR: *mut u32 = 0x0900_0000 as _;
unsafe fn uart_putc(c: u8) {
    unsafe {
        core::ptr::write_volatile(UART0_DR, c as u32);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> ! {
    unsafe {
        // Print "Hello world"
        let hello = b"Hello world";
        for &c in hello.iter() {
            uart_putc(c);
        }
    }

    loop {
        hint::spin_loop();
    }
}
