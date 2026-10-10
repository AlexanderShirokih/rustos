#![no_std]
#![no_main]

component_manifest! {
    exports: [
        hardware::Protocol,
        system_info::Protocol,
        image::Protocol,
        features::Protocol,
        journal::Protocol,
    ],
}

extern crate alloc;

use bootstrap_abi::{features, hardware, image, system_info};
use component_startup::component_manifest;
use kernel_objects::Port;
use log_abi::journal;
use rootkeeper::{Stopped, run};

#[program_entry::process]
fn start(kernel: &Port) -> Stopped {
    run(kernel.as_raw())
}
