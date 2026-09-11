#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::component_manifest;
use kernel_objects::Port;
use runtime::{Timeout, install_log, log_line, process_exit};
use syscall::{Handle, kind};

mod probe_protocol;

use probe_protocol::probe;

const LOG_TAG: &str = "idle";

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct IdleExportStartup {
        exports: [probe::Protocol],
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Ok(started) = IdleExportStartup::start(parent) else {
        process_exit(FAILED_EXIT_CODE)
    };
    if let Some(log) = started.log {
        install_log(log.into_handle().into_raw(), LOG_TAG);
    }
    log_line("started");

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
