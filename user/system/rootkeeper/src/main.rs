#![no_std]
#![no_main]

component_manifest! {
    exports: [hardware::Protocol, image::Protocol, features::Protocol],
}

extern crate alloc;

use bootstrap_abi::{features, hardware, image};
use component_startup::component_manifest;
use kernel_objects::Port;
use rootkeeper::{Stopped, run};

#[program_entry::process]
fn start(kernel: &Port) -> Stopped {
    run(kernel.as_raw())
}
