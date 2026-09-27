#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::component_manifest;
use kernel_objects::Port;
use runtime::{Timeout, log_line, process_exit, start_component};
use syscall::{Handle, kind};

mod probe_protocol;

use probe_protocol::probe;

const LOG_TAG: &str = "importer";

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct ImporterStartup {
        requirements: { probe: session(probe::Protocol) },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    let Ok(requirements) = ImporterStartup::request(parent) else {
        process_exit(FAILED_EXIT_CODE)
    };
    log_line("started");

    let _session = requirements.probe;
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
