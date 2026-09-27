#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::component_manifest;
use runtime::{log_line, process_exit, start_component};
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
    start_component(parent, LOG_TAG);
    let Ok(requirements) = IrqStartup::request(parent) else {
        process_exit(REFUSED_EXIT_CODE)
    };
    let _line = requirements.line;
    log_line("line taken");
    process_exit(TAKEN_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(REFUSED_EXIT_CODE)
}
