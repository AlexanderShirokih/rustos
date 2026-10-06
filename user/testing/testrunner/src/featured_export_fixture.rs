#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use alloc::boxed::Box;

use bootstrap_abi::{features, hardware, image};
use component_startup::{Attributes, Providers, Requirements as _, component_manifest};
use kernel_objects::Port;
use runtime::{log_fmt, log_line, start_component};

mod fixture_start;
mod probe_protocol;

use fixture_start::{Failed, fixture_start};
use probe_protocol::{probe, serve_probe};

const LOG_TAG: &str = "featured";

component_manifest! {
    struct FeaturedExportStartup {
        requirements: {
            hardware: all(hardware::Protocol),
            images: all(image::Protocol),
            features: all(features::Protocol),
        },
        features: ["testrunner,featured"],
        exports: [probe::Protocol => [{ kind = "featured" }]],
    }
}

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    start_component(parent.as_raw(), LOG_TAG);
    let requirements = FeaturedExportStartup::request(parent.as_raw()).map_err(|_| Failed)?;

    log_opened("hardware", &requirements.hardware)?;
    log_opened("image", &requirements.images)?;
    log_opened("features", &requirements.features)?;

    let export = Port::create().map_err(|_| Failed)?;
    log_line("started");
    serve_probe(parent.as_raw(), Box::leak(Box::new(export)))
}

fn log_opened<P: ipc::Protocol>(name: &str, providers: &Providers<P>) -> Result<(), Failed> {
    let found = providers
        .all(Attributes::empty())
        .map_err(|_| Failed)?
        .next()
        .ok_or(Failed)?
        .map_err(|_| Failed)?;
    let opened = providers
        .open(&found)
        .map(drop)
        .map_err(|error| error.failure);
    log_fmt(format_args!("{name} {opened:?}"));
    Ok(())
}
