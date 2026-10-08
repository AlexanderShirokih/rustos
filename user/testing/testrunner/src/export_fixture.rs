#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use alloc::boxed::Box;

use component_startup::component_manifest;
use kernel_objects::Port;
use runtime::{log_line, start_component};

mod fixture_start;
mod probe_protocol;

use fixture_start::{Failed, fixture_start};
use probe_protocol::{probe, serve_probe};

const LOG_TAG: &str = "export";

component_manifest! {
    exports: [probe::Protocol],
}

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    start_component(parent.as_raw(), LOG_TAG);
    let export = Port::create().map_err(|_| Failed)?;
    log_line("started");
    serve_probe(parent.as_raw(), Box::leak(Box::new(export)))
}
