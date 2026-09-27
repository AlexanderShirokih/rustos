#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{StartupFailure, component_manifest};
use runtime::process_exit;
use syscall::{Handle, kind};

mod probe_protocol;

use probe_protocol::probe;

const STARTED_EXIT_CODE: u64 = 0;
const FIRST_SESSION_REFUSED_EXIT_CODE: u64 = 1;
const SECOND_SESSION_REFUSED_EXIT_CODE: u64 = 2;
const FIRST_IPC_FAILED_EXIT_CODE: u64 = 3;
const SECOND_IPC_FAILED_EXIT_CODE: u64 = 4;
const FAILED_EXIT_CODE: u64 = 5;

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
    match TwoRequirementsStartup::request(parent) {
        Ok(_) => process_exit(STARTED_EXIT_CODE),
        Err(refusal) if refusal.requirement == "first" => process_exit(match refusal.failure {
            StartupFailure::Session(_) => FIRST_SESSION_REFUSED_EXIT_CODE,
            StartupFailure::Ipc(_) => FIRST_IPC_FAILED_EXIT_CODE,
            StartupFailure::Resource(_) | StartupFailure::NoStartHandle => FAILED_EXIT_CODE,
        }),
        Err(refusal) if refusal.requirement == "second" => process_exit(match refusal.failure {
            StartupFailure::Session(_) => SECOND_SESSION_REFUSED_EXIT_CODE,
            StartupFailure::Ipc(_) => SECOND_IPC_FAILED_EXIT_CODE,
            StartupFailure::Resource(_) | StartupFailure::NoStartHandle => FAILED_EXIT_CODE,
        }),
        Err(_) => process_exit(FAILED_EXIT_CODE),
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
