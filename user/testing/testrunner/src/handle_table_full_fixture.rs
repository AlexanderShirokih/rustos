#![no_std]
#![no_main]

use kernel_objects::Port;
use runtime::Timeout;

mod failed;
mod handle_table_fill;

use failed::Failed;
use handle_table_fill::fill_handle_table;

#[program_entry::process]
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
