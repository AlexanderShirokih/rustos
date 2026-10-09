#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;

use bootstrap_abi::{features, hardware, image};
use component_startup::{Attributes, Providers, Requirements as _, component_manifest};
use kernel_objects::Port;
use runtime::{log_fmt, log_line};
use syscall::{Handle, kind};

mod failed;
mod probe_protocol;

use failed::Failed;
use probe_protocol::{probe, serve_probe};

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

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let requirements = FeaturedExportStartup::from_parent(parent);

    log_opened("hardware", &requirements.hardware)?;
    log_opened("image", &requirements.images)?;
    log_opened("features", &requirements.features)?;

    let export = Port::create().map_err(|_| Failed)?;
    log_line("started");
    serve_probe(parent, Box::leak(Box::new(export)))
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
