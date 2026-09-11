#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use kernel_objects::{Port, kind};
use runtime::{Timeout, thread_exit};
use syscall::Handle;

#[unsafe(no_mangle)]
pub extern "C" fn _start(service: Handle<kind::Port>) -> ! {
    let port = Port::from_handle(service);
    let received = port.recv(Timeout::INFINITE);
    thread_exit(u64::from(received.is_err()))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
