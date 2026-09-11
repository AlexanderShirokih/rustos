#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::component_manifest;
use runtime::{install_log, log_line, process_exit};
use syscall::{Handle, kind};

const LOG_TAG: &str = "irq";

const REFUSED_EXIT_CODE: u64 = 3;
const TAKEN_EXIT_CODE: u64 = 4;

component_manifest! {
    struct IrqStartup {
        requirements: { line: irq(compatible = ["arm,pl031"]) },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Ok(started) = IrqStartup::start(parent) else {
        process_exit(REFUSED_EXIT_CODE)
    };
    if let Some(log) = started.log {
        install_log(log.into_handle().into_raw(), LOG_TAG);
    }
    let _line = started.requirements.line;
    log_line("line taken");
    process_exit(TAKEN_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(REFUSED_EXIT_CODE)
}
