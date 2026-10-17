#![no_std]
#![no_main]

extern crate alloc;

use application_abi::application;
use application_window::accept_window;
use component_startup::{component_manifest, component_startup as startup_contract};
use kernel_objects::PortTransport;
use syscall::{Handle, kind};

mod failed;

use failed::Failed;

component_manifest! {
    exports: [application::Protocol],
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let shown = accept_window(startup_contract::Client::new(PortTransport::new(parent)))
        .map_err(|_| Failed)?;
    shown.session.serve();
    Ok(())
}
