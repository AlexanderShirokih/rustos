#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{Requirements as _, component_manifest};
use runtime::{log_line, process_exit, start_component};
use syscall::{Handle, kind};

const LOG_TAG: &str = "failing";

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct FailingStartup {}
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    if FailingStartup::request(parent).is_err() {
        process_exit(FAILED_EXIT_CODE)
    }
    log_line("started");
    process_exit(FAILED_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
