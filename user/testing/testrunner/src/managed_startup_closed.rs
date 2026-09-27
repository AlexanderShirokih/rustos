#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::component_manifest;
use kernel_objects::Port;
use runtime::{Timeout, handle_close, log_line, process_exit, start_component};
use syscall::{Handle, kind};

const LOG_TAG: &str = "closed";

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct ClosedStartup {}
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    if ClosedStartup::request(parent).is_err() {
        process_exit(FAILED_EXIT_CODE)
    }
    log_line("started");
    if handle_close(parent) != 0 {
        process_exit(FAILED_EXIT_CODE)
    }

    let Ok(own) = Port::create() else {
        process_exit(FAILED_EXIT_CODE)
    };
    let _ = own.recv(Timeout::INFINITE);
    process_exit(FAILED_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
