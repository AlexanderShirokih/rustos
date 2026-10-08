#![no_std]
#![no_main]
#![allow(unsafe_code)]

use bootstrap_abi::bootstrap;
use kernel_objects::Port;
use log_abi::log;
use runtime::PortTransport;

mod fixture_start;
mod log_fixture_protocol;

use fixture_start::{Failed, fixture_start};
use log_fixture_protocol::FIXTURE_LINE;

fixture_start!(probe);

fn probe(log: &Port) -> Result<(), Failed> {
    let bootstrap = bootstrap::Client::new(PortTransport::new(log.borrow()));
    if bootstrap.acquire_hardware().is_ok() {
        return Err(Failed);
    }
    if bootstrap.acquire_userland_image().is_ok() {
        return Err(Failed);
    }

    let client = log::Client::new(PortTransport::new(log.borrow()));
    client.write_str("probe", FIXTURE_LINE).map_err(|_| Failed)
}
