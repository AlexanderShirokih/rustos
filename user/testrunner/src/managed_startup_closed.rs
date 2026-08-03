#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use runtime::{OwnedHandle, thread_exit};
use syscall::Handle;

#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    if OwnedHandle::from_handle(startup).close().is_err() {
        thread_exit(u64::from(u32::MAX));
    }
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
