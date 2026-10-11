#![no_std]
#![no_main]

use filesystem_abi::{AccessMode, OpenFlags, file_system};
use kernel_objects::Port;
use runtime::PortTransport;

mod failed;

use failed::Failed;

const BUFFER_SIZE: u32 = 64 * 1024;

const PATH: &str = "/fixture";

#[program_entry::process]
fn populate_session(service: &Port) -> Result<(), Failed> {
    let client = file_system::Client::new(PortTransport::new(service));

    let _buffer = client.acquire_buffer(BUFFER_SIZE).map_err(|_| Failed)?;

    let path = PATH;
    client
        .open(path, AccessMode::ReadWrite, OpenFlags::CREATE)
        .map_err(|_| Failed)?;
    Ok(())
}
