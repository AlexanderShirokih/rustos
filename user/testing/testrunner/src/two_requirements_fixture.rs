#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{StartupFailure, component_manifest};
use runtime::process_exit;
use syscall::{Handle, kind};

mod probe_protocol;
mod startup_exit_protocol;

use probe_protocol::probe;
use startup_exit_protocol::{FIRST_IPC_FAILED_EXIT_CODE, SECOND_SESSION_REFUSED_EXIT_CODE};

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct TwoRequirementsStartup {
        requirements: {
            first: session(probe::Protocol),
            second: session(probe::Protocol),
        },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let code = match TwoRequirementsStartup::request(parent) {
        Err(refusal)
            if refusal.requirement == "second"
                && matches!(refusal.failure, StartupFailure::Session(_)) =>
        {
            u64::from(SECOND_SESSION_REFUSED_EXIT_CODE)
        }
        Err(refusal)
            if refusal.requirement == "first"
                && matches!(refusal.failure, StartupFailure::Ipc(_)) =>
        {
            u64::from(FIRST_IPC_FAILED_EXIT_CODE)
        }
        _ => FAILED_EXIT_CODE,
    };
    process_exit(code)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
