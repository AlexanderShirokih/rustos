#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use kernel_objects::{Port, kind};
use runtime::thread_exit;
use syscall::Handle;

#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle<kind::Port>) -> ! {
    if Port::from_handle(startup).close().is_err() {
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
