#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;

use component_startup::{SessionPort, component_manifest};
use runtime::log_line;
use syscall::{Handle, kind};

mod failed;
mod probe_protocol;

use failed::Failed;
use probe_protocol::{probe, serve_probe};

component_manifest! {
    exports: [probe::Protocol],
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let export = SessionPort::create().map_err(|_| Failed)?;
    log_line("started");
    serve_probe(parent, Box::leak(Box::new(export)))
}
