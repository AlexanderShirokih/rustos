#![no_std]
#![no_main]
#![allow(unsafe_code)]

use kernel_objects::{IssuedHandle, Port, kind};
use runtime::Timeout;

mod fixture_start;

use fixture_start::{Failed, fixture_start};

fixture_start!(run);

fn run(client: IssuedHandle<kind::Port>) -> Result<(), Failed> {
    Port::from(client)
        .send_bytes(b"badged", Timeout::INFINITE)
        .map_err(|_| Failed)
}
