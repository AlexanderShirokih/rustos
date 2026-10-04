#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use alloc::boxed::Box;

use component_startup::{Requirements as _, component_manifest};
use kernel_objects::Port;
use runtime::{log_line, start_component};

mod fixture_start;
mod probe_protocol;

use fixture_start::{Failed, fixture_start};
use probe_protocol::{probe, serve_probe};

const LOG_TAG: &str = "filtered-export";

component_manifest! {
    struct FilteredExportStartup {
        exports: [probe::Protocol => [
            { compatible = "a" },
            { compatible = "b", role = "c" },
            { role = "c" },
        ]],
    }
}

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    start_component(parent.as_raw(), LOG_TAG);
    FilteredExportStartup::request(parent.as_raw()).map_err(|_| Failed)?;

    let export = Port::create().map_err(|_| Failed)?;
    log_line("started");
    serve_probe(parent.as_raw(), Box::leak(Box::new(export)))
}
