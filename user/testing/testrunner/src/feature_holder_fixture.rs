#![no_std]
#![no_main]
#![allow(unsafe_code)]

use bootstrap_abi::features;
use component_startup::{Requirements as _, component_manifest};
use kernel_objects::Port;
use runtime::{Timeout, log_line, start_component};

mod fixture_start;

use fixture_start::{Failed, fixture_start};

const LOG_TAG: &str = "holder";

component_manifest! {
    struct FeatureHolderStartup {
        requirements: { features: session(features::Protocol) },
    }
}

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    start_component(parent.as_raw(), LOG_TAG);
    // Открытая сессия `Features` оставляет публикацию незавершённой до конца процесса.
    let _features = FeatureHolderStartup::from_parent(parent.as_raw())
        .features
        .open()
        .map_err(|_| Failed)?;
    log_line("started");

    let idle = Port::create().map_err(|_| Failed)?;
    let _ = idle.recv(Timeout::INFINITE);
    Err(Failed)
}
