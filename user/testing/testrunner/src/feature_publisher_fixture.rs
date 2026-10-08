#![no_std]
#![no_main]
#![allow(unsafe_code)]

use bootstrap_abi::features;
use component_startup::{Requirements as _, component_manifest};
use ipc::wire::Str;
use kernel_objects::Port;
use runtime::{Timeout, log_fmt, start_component};

mod fixture_start;

use fixture_start::{Failed, fixture_start};

const LOG_TAG: &str = "publisher";

const FEATURE: &str = "testrunner,featured";

component_manifest! {
    struct FeaturePublisherStartup {
        requirements: { features: session(features::Protocol) },
    }
}

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    start_component(parent.as_raw(), LOG_TAG);
    let features = FeaturePublisherStartup::from_parent(parent.as_raw())
        .features
        .open()
        .map_err(|_| Failed)?
        .client();
    let feature = Str::new(FEATURE).ok_or(Failed)?;

    let first = features.publish(feature);
    let again = features.publish(feature);
    // Закрытие сессии `Features` завершает публикацию, и rootkeeper запускает удержанные записи.
    drop(features);
    log_fmt(format_args!("published {first:?} {again:?}"));

    let idle = Port::create().map_err(|_| Failed)?;
    let _ = idle.recv(Timeout::INFINITE);
    Err(Failed)
}
