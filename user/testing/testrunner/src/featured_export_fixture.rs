#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use core::fmt::Debug;

use bootstrap_abi::{features, hardware, image, system_info};
use component_startup::{
    Attributes, Providers, Requirements as _, SessionPort, component_manifest,
};
use kernel_objects::Session;
use log_abi::journal;
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
            system_info: all(system_info::Protocol),
            images: all(image::Protocol),
            features: all(features::Protocol),
            journal: all(journal::Protocol),
        },
        features: ["testrunner,featured"],
        exports: [probe::Protocol => [{ kind = "featured" }]],
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let requirements = FeaturedExportStartup::from_parent(parent);

    log_opened("hardware", &requirements.hardware, drop)?;
    log_opened("system_info", &requirements.system_info, |session| {
        session.client().snapshot().is_ok_and(|snapshot| {
            snapshot.cpus > 0 && snapshot.free_frames <= snapshot.managed_frames
        })
    })?;
    log_opened("image", &requirements.images, drop)?;
    log_opened("features", &requirements.features, drop)?;
    log_opened("journal", &requirements.journal, drop)?;

    let export = SessionPort::create().map_err(|_| Failed)?;
    log_line("started");
    serve_probe(parent, Box::leak(Box::new(export)))
}

fn log_opened<P: ipc::Protocol, T: Debug>(
    name: &str,
    providers: &Providers<P>,
    inspect: impl FnOnce(Session<P>) -> T,
) -> Result<(), Failed> {
    let found = providers
        .all(Attributes::empty())
        .map_err(|_| Failed)?
        .next()
        .ok_or(Failed)?
        .map_err(|_| Failed)?;
    let opened = providers
        .open(&found)
        .map(inspect)
        .map_err(|error| error.failure);
    log_fmt(format_args!("{name} {opened:?}"));
    Ok(())
}
