#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::component_manifest;
use kernel_objects::Port;
use runtime::{Timeout, handle_close, install_log, log_line, process_exit};
use syscall::{Handle, kind};

const LOG_TAG: &str = "closed";

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct ClosedStartup {}
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Ok(started) = ClosedStartup::start(parent) else {
        process_exit(FAILED_EXIT_CODE)
    };
    if let Some(log) = started.log {
        install_log(log.into_handle().into_raw(), LOG_TAG);
        log_line("started");
    }
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
