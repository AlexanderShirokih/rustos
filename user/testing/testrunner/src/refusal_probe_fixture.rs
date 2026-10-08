#![no_std]
#![no_main]
#![allow(unsafe_code)]

use component_abi::{LOG_REQUIREMENT, component_startup as startup_contract};
use component_startup::{Attributes, Requirements as _, accept, component_manifest, session};
use kernel_objects::Port;
use rtc_abi::rtc;
use runtime::{PortTransport, Timeout, log_fmt, start_component};

mod fixture_start;
mod probe_protocol;

use fixture_start::{Failed, fixture_start};
use probe_protocol::probe;

const LOG_TAG: &str = "refusals";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(1);

const PROBE_REQUIREMENT: &str = "probe";
const OUTSIDE_REQUIREMENT: &str = "outside";

component_manifest! {
    struct RefusalProbeStartup {
        requirements: { probe: session(probe::Protocol) },
    }
}

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    start_component(parent.as_raw(), LOG_TAG);
    let _probe = RefusalProbeStartup::from_parent(parent.as_raw())
        .probe
        .open()
        .map_err(|_| Failed)?;

    let client = startup_contract::Client::new(PortTransport::new(parent)).with_wait(CALL_TIMEOUT);
    let outside = session::<probe::Protocol>(
        parent.as_raw(),
        OUTSIDE_REQUIREMENT,
        Attributes::empty(),
        CALL_TIMEOUT,
    )
    .map(drop);
    log_fmt(format_args!("outside {outside:?}"));
    let protocol = session::<rtc::Protocol>(
        parent.as_raw(),
        PROBE_REQUIREMENT,
        Attributes::empty(),
        CALL_TIMEOUT,
    )
    .map(drop);
    log_fmt(format_args!("protocol {protocol:?}"));
    let log_protocol = session::<rtc::Protocol>(
        parent.as_raw(),
        LOG_REQUIREMENT,
        Attributes::empty(),
        CALL_TIMEOUT,
    )
    .map(drop);
    log_fmt(format_args!("log {log_protocol:?}"));
    let export = accept::<rtc::Protocol>(&client).map(drop);
    log_fmt(format_args!("accept {export:?}"));
    Ok(())
}
