#![no_std]
#![no_main]
#![allow(unsafe_code)]

use component_startup::{Requirements as _, component_manifest, serve_export, whole_export};
use filesystem_abi::file_system;
use kernel_objects::Port;

mod fixture_start;

use fixture_start::{Failed, fixture_start};

component_manifest! {
    struct UnresponsiveFileSystemStartup {
        requirements: {},
        exports: [file_system::Protocol],
    }
}

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    let requirements = UnresponsiveFileSystemStartup::from_parent(parent.as_raw());
    let service = Port::create().map_err(|_| Failed)?;
    let _ = serve_export::<file_system::Protocol>(&requirements.parent, whole_export(&service));
    Err(Failed)
}
