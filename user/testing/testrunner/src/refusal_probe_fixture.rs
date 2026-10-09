#![no_std]
#![no_main]

use component_abi::{LOG_REQUIREMENT, component_startup as startup_contract};
use component_startup::{Attributes, Requirements as _, accept, component_manifest, session};
use rtc_abi::rtc;
use runtime::{PortTransport, Timeout, log_fmt};
use syscall::{Handle, kind};

mod failed;
mod probe_protocol;

use failed::Failed;
use probe_protocol::probe;

const CALL_TIMEOUT: Timeout = Timeout::from_secs(1);

const PROBE_REQUIREMENT: &str = "probe";
const OUTSIDE_REQUIREMENT: &str = "outside";

component_manifest! {
    struct RefusalProbeStartup {
        requirements: { probe: session(probe::Protocol) },
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let _probe = RefusalProbeStartup::from_parent(parent)
        .probe
        .open()
        .map_err(|_| Failed)?;

    let client = startup_contract::Client::new(PortTransport::new(parent)).with_wait(CALL_TIMEOUT);
    let outside = session::<probe::Protocol>(
        parent,
        OUTSIDE_REQUIREMENT,
        Attributes::empty(),
        CALL_TIMEOUT,
    )
    .map(drop);
    log_fmt(format_args!("outside {outside:?}"));
    let protocol =
        session::<rtc::Protocol>(parent, PROBE_REQUIREMENT, Attributes::empty(), CALL_TIMEOUT)
            .map(drop);
    log_fmt(format_args!("protocol {protocol:?}"));
    let log_protocol =
        session::<rtc::Protocol>(parent, LOG_REQUIREMENT, Attributes::empty(), CALL_TIMEOUT)
            .map(drop);
    log_fmt(format_args!("log {log_protocol:?}"));
    let export = accept::<rtc::Protocol>(&client).map(drop);
    log_fmt(format_args!("accept {export:?}"));
    Ok(())
}
