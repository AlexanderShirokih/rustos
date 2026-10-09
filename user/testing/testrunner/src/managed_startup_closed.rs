#![no_std]
#![no_main]

use component_startup::{Requirements as _, component_manifest};
use kernel_objects::Port;
use runtime::{ExitCode, FAILED_EXIT_CODE, Timeout, log_line, start_component};

mod probe_protocol;

use probe_protocol::probe;

component_manifest! {
    struct ClosedStartup {
        requirements: { probe: session(probe::Protocol) },
    }
}

#[program_entry::process]
fn run(parent: Port) -> ExitCode {
    start_component(parent.as_raw());
    // Значение строится без открытия требования, и родитель не получает запроса.
    let _requirements = ClosedStartup::from_parent(parent.as_raw());
    log_line("started");
    if parent.close().is_ok()
        && let Ok(own) = Port::create()
    {
        let _ = own.recv(Timeout::INFINITE);
    }
    ExitCode::new(FAILED_EXIT_CODE)
}
