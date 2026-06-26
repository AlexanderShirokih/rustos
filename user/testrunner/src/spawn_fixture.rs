#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use runtime::{handle_close, thread_exit};
use syscall::Handle;

/// Программа-фикстура для теста `spawn_from_image`.
#[unsafe(no_mangle)]
pub extern "C" fn _start(start: Handle) -> ! {
    let code = u64::from(handle_close(start) != 0);
    thread_exit(code)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
