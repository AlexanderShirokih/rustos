#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;

use component_startup::component_manifest;
use kernel_objects::Port;
use runtime::log_line;
use syscall::{Handle, kind};

mod failed;
mod probe_protocol;

use failed::Failed;
use probe_protocol::{probe, serve_probe};

component_manifest! {
    exports: [probe::Protocol => [
        { kind = "a" },
        { kind = "b", role = "c" },
        { role = "c" },
    ]],
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let export = Port::create().map_err(|_| Failed)?;
    log_line("started");
    serve_probe(parent, Box::leak(Box::new(export)))
}
