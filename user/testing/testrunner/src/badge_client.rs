#![no_std]
#![no_main]
#![allow(unsafe_code)]

use kernel_objects::Port;
use runtime::Timeout;

mod fixture_start;

use fixture_start::{Failed, fixture_start};

fixture_start!(run);

fn run(client: &Port) -> Result<(), Failed> {
    client
        .send_bytes(b"badged", Timeout::INFINITE)
        .map_err(|_| Failed)
}
