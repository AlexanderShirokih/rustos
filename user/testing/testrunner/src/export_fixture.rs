#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use alloc::boxed::Box;
use core::panic::PanicInfo;

use component_startup::component_manifest;
use kernel_objects::Port;
use runtime::{install_log, log_line, process_exit};
use syscall::{Handle, kind};

mod probe_protocol;

use probe_protocol::{probe, serve_probe};

const LOG_TAG: &str = "export";

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct ExportStartup {
        exports: [probe::Protocol],
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Ok(started) = ExportStartup::start(parent) else {
        process_exit(FAILED_EXIT_CODE)
    };
    if let Some(log) = started.log {
        install_log(log.into_handle().into_raw(), LOG_TAG);
    }
    let Ok(export) = Port::create() else {
        process_exit(FAILED_EXIT_CODE)
    };
    log_line("started");
    serve_probe(parent, Box::leak(Box::new(export)))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
