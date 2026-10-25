#![no_std]
#![no_main]

use bootstrap_abi::features;
use component_startup::{Requirements as _, component_manifest};
use kernel_objects::Port;
use runtime::{Timeout, log_fmt};
use syscall::{Handle, kind};

mod failed;

use failed::Failed;

const FEATURE: &str = "testrunner,featured";

component_manifest! {
    struct FeaturePublisherStartup {
        requirements: { features: session(features::Protocol) },
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let features = FeaturePublisherStartup::from_parent(parent)
        .features
        .open()
        .map_err(|_| Failed)?
        .client();
    let feature = FEATURE;

    let first = features.publish(feature).wait();
    let again = features.publish(feature).wait();
    // Закрытие сессии `Features` завершает публикацию, и rootkeeper запускает удержанные записи.
    drop(features);
    log_fmt(format_args!("published {first:?} {again:?}"));

    let idle = Port::create().map_err(|_| Failed)?;
    let _ = idle.recv(Timeout::INFINITE);
    Err(Failed)
}
