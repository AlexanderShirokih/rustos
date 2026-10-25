#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;

use component_startup::{SessionPort, component_manifest};
use executor::Executor;
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
    let executor = Rc::new(Executor::new().map_err(|_| Failed)?);
    executor.block_on(serve_probe(&executor, parent, export))
}
