#![no_std]
#![no_main]

use component_startup::component_manifest;
use kernel_objects::Port;
use runtime::{Timeout, log_line};
use syscall::{Handle, kind};

mod failed;
mod probe_protocol;

use failed::Failed;
use probe_protocol::probe;

component_manifest! {
    exports: [probe::Protocol],
}

#[program_entry::component]
fn run(_parent: Handle<kind::Port>) -> Result<(), Failed> {
    log_line("started");

    let own = Port::create().map_err(|_| Failed)?;
    let _ = own.recv(Timeout::INFINITE);
    Err(Failed)
}
