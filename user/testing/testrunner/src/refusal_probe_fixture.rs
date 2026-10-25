#![no_std]
#![no_main]

use component_abi::LOG_REQUIREMENT;
use component_startup::{
    AsyncStartup, Attributes, Requirements as _, accept, component_manifest, session_async,
};
use executor::{Executor, PortChannel};
use rtc_abi::rtc;
use runtime::{Timeout, log_fmt};
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
    let executor = Executor::new().map_err(|_| Failed)?;
    let reactor = executor.reactor();

    executor.block_on(async {
        let _probe = RefusalProbeStartup::from_parent(parent)
            .probe
            .open_async(&reactor)
            .await
            .map_err(|_| Failed)?;

        let client =
            AsyncStartup::new(PortChannel::new(parent, reactor.clone())).with_wait(CALL_TIMEOUT);
        let outside = session_async::<probe::Protocol>(
            parent,
            &reactor,
            OUTSIDE_REQUIREMENT,
            Attributes::empty(),
            CALL_TIMEOUT,
        )
        .await
        .map(drop);
        log_fmt(format_args!("outside {outside:?}"));
        let protocol = session_async::<rtc::Protocol>(
            parent,
            &reactor,
            PROBE_REQUIREMENT,
            Attributes::empty(),
            CALL_TIMEOUT,
        )
        .await
        .map(drop);
        log_fmt(format_args!("protocol {protocol:?}"));
        let log_protocol = session_async::<rtc::Protocol>(
            parent,
            &reactor,
            LOG_REQUIREMENT,
            Attributes::empty(),
            CALL_TIMEOUT,
        )
        .await
        .map(drop);
        log_fmt(format_args!("log {log_protocol:?}"));
        let export = accept::<rtc::Protocol>(&client).await.map(drop);
        log_fmt(format_args!("accept {export:?}"));
        Ok(())
    })
}
