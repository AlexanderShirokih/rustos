#![no_std]
#![no_main]
#![allow(unsafe_code)]

use filesystem_abi::{AccessMode, OpenFlags, file_system};
use ipc::wire::Str;
use kernel_objects::Port;
use runtime::PortTransport;
use syscall::Timeout;

mod fixture_start;

use fixture_start::{Failed, fixture_start};

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const BUFFER_SIZE: u32 = 64 * 1024;

const PATH: &str = "/fixture";

fixture_start!(populate_session);

fn populate_session(service: &Port) -> Result<(), Failed> {
    let client = file_system::Client::new(PortTransport::new(service)).with_wait(CALL_TIMEOUT);

    let _buffer = client.acquire_buffer(BUFFER_SIZE).map_err(|_| Failed)?;

    let path = Str::new(PATH).ok_or(Failed)?;
    client
        .open(path, AccessMode::ReadWrite, OpenFlags::CREATE)
        .map_err(|_| Failed)?;
    Ok(())
}
