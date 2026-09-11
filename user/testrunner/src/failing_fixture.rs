#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::component_manifest;
use runtime::{install_log, log_line, process_exit};
use syscall::{Handle, kind};

const LOG_TAG: &str = "failing";

const FAILED_EXIT_CODE: u64 = 1;
const FAILING_EXIT_CODE: u64 = 5;

component_manifest! {
    struct FailingStartup {}
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Ok(started) = FailingStartup::start(parent) else {
        process_exit(FAILED_EXIT_CODE)
    };
    if let Some(log) = started.log {
        install_log(log.into_handle().into_raw(), LOG_TAG);
    }
    log_line("started");
    process_exit(FAILING_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
