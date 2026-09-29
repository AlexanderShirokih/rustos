#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use kernel_objects::Port;
use runtime::{Timeout, take_bootstrap, thread_exit};

#[unsafe(no_mangle)]
pub extern "C" fn _start(service: u32) -> ! {
    // SAFETY: `service` - значение `x0` точки входа, и точка входа исполняется один раз.
    unsafe { runtime::start(service) };
    let Some(service) = take_bootstrap() else {
        thread_exit(1)
    };
    let port = Port::from(service.assume_kind());
    let received = port.recv(Timeout::INFINITE);
    thread_exit(u64::from(received.is_err()))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
