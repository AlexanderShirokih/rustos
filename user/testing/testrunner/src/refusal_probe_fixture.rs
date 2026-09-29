#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_abi::{LOG_REQUIREMENT, component_startup as startup_contract};
use component_startup::{accept, component_manifest, session};
use ipc::wire::Str;
use rtc_abi::rtc;
use runtime::{PortTransport, Timeout, log_fmt, process_exit, start_component};
use syscall::{Handle, kind};

mod probe_protocol;

use probe_protocol::probe;

const LOG_TAG: &str = "refusals";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(1);

const PROBE_REQUIREMENT: &str = "probe";
const OUTSIDE_REQUIREMENT: &str = "outside";

const CHECKED_EXIT_CODE: u64 = 0;
const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct RefusalProbeStartup {
        requirements: { probe: session(probe::Protocol) },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    if RefusalProbeStartup::request(parent).is_err() {
        process_exit(FAILED_EXIT_CODE)
    }
    let Some(probe_requirement) = Str::new(PROBE_REQUIREMENT) else {
        process_exit(FAILED_EXIT_CODE)
    };

    let client = startup_contract::Client::new(PortTransport::new(parent)).with_wait(CALL_TIMEOUT);
    let outside =
        session::<probe::Protocol>(&client, OUTSIDE_REQUIREMENT).map(|answer| answer.map(drop));
    log_fmt(format_args!("outside {outside:?}"));
    let protocol =
        session::<rtc::Protocol>(&client, PROBE_REQUIREMENT).map(|answer| answer.map(drop));
    log_fmt(format_args!("protocol {protocol:?}"));
    let log_protocol =
        session::<rtc::Protocol>(&client, LOG_REQUIREMENT).map(|answer| answer.map(drop));
    log_fmt(format_args!("log {log_protocol:?}"));
    let region = client
        .region(probe_requirement)
        .map(|answer| answer.map(drop));
    log_fmt(format_args!("region {region:?}"));
    let export = accept::<rtc::Protocol>(&client).map(|answer| answer.map(drop));
    log_fmt(format_args!("accept {export:?}"));
    process_exit(CHECKED_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
