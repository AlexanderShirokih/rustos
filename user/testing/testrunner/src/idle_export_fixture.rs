#![no_std]
#![no_main]
#![allow(unsafe_code)]

use component_startup::{Requirements as _, component_manifest};
use kernel_objects::Port;
use runtime::{Timeout, log_line, start_component};

mod fixture_start;
mod probe_protocol;

use fixture_start::{Failed, fixture_start};
use probe_protocol::probe;

const LOG_TAG: &str = "idle";

component_manifest! {
    struct IdleExportStartup {
        exports: [probe::Protocol],
    }
}

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    start_component(parent.as_raw(), LOG_TAG);
    IdleExportStartup::request(parent.as_raw()).map_err(|_| Failed)?;
    log_line("started");

    let own = Port::create().map_err(|_| Failed)?;
    let _ = own.recv(Timeout::INFINITE);
    Err(Failed)
}
