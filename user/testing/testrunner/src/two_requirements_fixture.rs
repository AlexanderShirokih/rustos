#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{Requirements as _, StartupError, StartupFailure, component_manifest};
use runtime::{Timeout, process_exit};
use syscall::{Handle, kind};

mod probe_protocol;
mod startup_exit_protocol;

use probe_protocol::probe;
use startup_exit_protocol::{FIRST_IPC_FAILED_EXIT_CODE, SECOND_SESSION_REFUSED_EXIT_CODE};

const FAILED_EXIT_CODE: u64 = 1;
const PING: u64 = 7;
const PING_TIMEOUT: Timeout = Timeout::from_secs(2);

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
    process_exit(exit_code(&TwoRequirementsStartup::from_parent(parent)))
}

fn exit_code(requirements: &TwoRequirementsStartup) -> u64 {
    let first = match requirements.first.open() {
        Ok(first) => first.client().with_wait(PING_TIMEOUT),
        Err(StartupError {
            requirement: "first",
            failure: StartupFailure::Ipc(_),
        }) => return u64::from(FIRST_IPC_FAILED_EXIT_CODE),
        Err(_) => return FAILED_EXIT_CODE,
    };
    match requirements.second.open() {
        Err(StartupError {
            requirement: "second",
            failure: StartupFailure::Refused,
        }) if first.ping(PING) == Ok(PING) => u64::from(SECOND_SESSION_REFUSED_EXIT_CODE),
        _ => FAILED_EXIT_CODE,
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
