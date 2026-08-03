#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use syscall::Handle;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_startup: Handle) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
