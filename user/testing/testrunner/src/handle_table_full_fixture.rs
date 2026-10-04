#![no_std]
#![no_main]
#![allow(unsafe_code)]

use kernel_objects::Port;
use runtime::Timeout;

mod fixture_start;
mod handle_table_fill;

use fixture_start::{Failed, fixture_start};
use handle_table_fill::fill_handle_table;

fixture_start!(run);

fn run(server: &Port) -> Result<(), Failed> {
    let parking = Port::create().map_err(|_| Failed)?;

    let spare = fill_handle_table().ok_or(Failed)?;
    drop(spare);

    if server.recv(Timeout::INFINITE).is_ok() {
        return Err(Failed);
    }

    let _ = parking.recv(Timeout::INFINITE);
    Err(Failed)
}
