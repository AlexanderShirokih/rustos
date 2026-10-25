#![no_std]
#![no_main]

use bootstrap_abi::bootstrap;
use kernel_objects::Port;
use log_abi::log;
use runtime::PortTransport;

mod failed;
mod log_fixture_protocol;

use failed::Failed;
use log_fixture_protocol::FIXTURE_LINE;

#[program_entry::process]
fn probe(log: &Port) -> Result<(), Failed> {
    let bootstrap = bootstrap::Client::new(PortTransport::new(log.borrow()));
    if bootstrap.acquire_hardware().wait().is_ok() {
        return Err(Failed);
    }
    if bootstrap.acquire_userland_image().wait().is_ok() {
        return Err(Failed);
    }

    let client = log::Client::new(PortTransport::new(log.borrow()));
    client.write_str(FIXTURE_LINE).map_err(|_| Failed)
}
