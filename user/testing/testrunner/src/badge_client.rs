#![no_std]
#![no_main]
#![allow(unsafe_code)]

use kernel_objects::{Port, kind};
use runtime::Timeout;
use syscall::Handle;

mod fixture_start;

use fixture_start::{Failed, fixture_start};

fixture_start!(run);

fn run(client: Handle<kind::Port>) -> Result<(), Failed> {
    Port::from_handle(client)
        .send_bytes(b"badged", Timeout::INFINITE)
        .map_err(|_| Failed)
}
