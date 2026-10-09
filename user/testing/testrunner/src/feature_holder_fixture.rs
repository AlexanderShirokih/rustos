#![no_std]
#![no_main]

use bootstrap_abi::features;
use component_startup::{Requirements as _, component_manifest};
use kernel_objects::Port;
use runtime::{Timeout, log_line};
use syscall::{Handle, kind};

mod failed;

use failed::Failed;

component_manifest! {
    struct FeatureHolderStartup {
        requirements: { features: session(features::Protocol) },
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    // Открытая сессия `Features` оставляет публикацию незавершённой до конца процесса.
    let _features = FeatureHolderStartup::from_parent(parent)
        .features
        .open()
        .map_err(|_| Failed)?;
    log_line("started");

    let idle = Port::create().map_err(|_| Failed)?;
    let _ = idle.recv(Timeout::INFINITE);
    Err(Failed)
}
