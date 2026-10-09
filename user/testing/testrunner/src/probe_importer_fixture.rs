#![no_std]
#![no_main]

use component_startup::{Requirements as _, component_manifest};
use kernel_objects::Port;
use runtime::{Timeout, log_line};
use syscall::{Handle, kind};

mod failed;
mod probe_protocol;

use failed::Failed;
use probe_protocol::probe;

component_manifest! {
    struct ImporterStartup {
        requirements: { probe: session(probe::Protocol) },
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let _session = ImporterStartup::from_parent(parent)
        .probe
        .open()
        .map_err(|_| Failed)?;
    log_line("started");

    let own = Port::create().map_err(|_| Failed)?;
    let _ = own.recv(Timeout::INFINITE);
    Err(Failed)
}
