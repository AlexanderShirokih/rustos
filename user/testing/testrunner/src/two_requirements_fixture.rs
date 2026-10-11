#![no_std]
#![no_main]

use component_startup::{StartupError, StartupFailure, component_manifest};
use runtime::{ExitCode, FAILED_EXIT_CODE};

mod probe_protocol;
mod startup_exit_protocol;

use probe_protocol::probe;
use startup_exit_protocol::{FIRST_IPC_FAILED_EXIT_CODE, SECOND_SESSION_REFUSED_EXIT_CODE};

const PING: u64 = 7;

component_manifest! {
    struct TwoRequirementsStartup {
        requirements: {
            first: session(probe::Protocol),
            second: session(probe::Protocol),
        },
    }
}

#[program_entry::component]
fn run(requirements: TwoRequirementsStartup) -> ExitCode {
    let TwoRequirementsStartup { first, second, .. } = requirements;
    let first = match first.open() {
        Ok(first) => first.client(),
        Err(StartupError {
            requirement: "first",
            failure: StartupFailure::Ipc(_),
        }) => return ExitCode::from(FIRST_IPC_FAILED_EXIT_CODE),
        Err(_) => return ExitCode::new(FAILED_EXIT_CODE),
    };
    match second.open() {
        Err(StartupError {
            requirement: "second",
            failure: StartupFailure::Refused,
        }) if first.ping(PING) == Ok(PING) => ExitCode::from(SECOND_SESSION_REFUSED_EXIT_CODE),
        _ => ExitCode::new(FAILED_EXIT_CODE),
    }
}
