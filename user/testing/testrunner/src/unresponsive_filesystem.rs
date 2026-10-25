#![no_std]
#![no_main]

use component_startup::{
    AsyncStartup, Requirements as _, SessionPort, component_manifest, serve_export, whole_export,
};
use executor::{Executor, PortChannel};
use filesystem_abi::file_system;
use syscall::{Handle, kind};

mod failed;

use failed::Failed;

component_manifest! {
    struct UnresponsiveFileSystemStartup {
        requirements: {},
        exports: [file_system::Protocol => [{ fs = "unresponsive" }]],
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let requirements = UnresponsiveFileSystemStartup::from_parent(parent);
    let service = SessionPort::create().map_err(|_| Failed)?;
    let executor = Executor::new().map_err(|_| Failed)?;
    let parent = AsyncStartup::new(PortChannel::from_transport(
        requirements.parent.into_channel(),
        executor.reactor(),
    ));

    let _ = executor.block_on(serve_export::<file_system::Protocol>(
        parent,
        whole_export(service),
    ));
    Err(Failed)
}
