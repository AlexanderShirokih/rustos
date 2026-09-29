#![no_std]
#![no_main]
#![allow(unsafe_code)]

use bootstrap_abi::bootstrap;
use kernel_objects::{MemoryRegion, kind};
use runtime::{PortTransport, handle_close};
use syscall::{HANDLE_TABLE_CAPACITY, Handle};

mod fixture_start;
mod handle_table_fill;

use fixture_start::{Failed, fixture_start};
use handle_table_fill::fill_handle_table;

fixture_start!(run);

fn run(kernel: Handle<kind::Port>) -> Result<(), Failed> {
    let client = bootstrap::Client::new(PortTransport::new(kernel));
    let spare = fill_handle_table().ok_or(Failed)?;

    for _ in 0..=HANDLE_TABLE_CAPACITY {
        if client.acquire_userland_image().is_ok() {
            return Err(Failed);
        }
    }

    let _ = handle_close(spare);
    let image = client
        .acquire_userland_image()
        .map_err(|_| Failed)?
        .map_err(|_| Failed)?;
    drop(MemoryRegion::from(image));
    Ok(())
}
