#![no_std]
#![no_main]

use kernel_objects::Port;
use runtime::Timeout;

mod failed;

use failed::Failed;

#[program_entry::process]
fn run(client: &Port) -> Result<(), Failed> {
    client
        .send_bytes(b"badged", Timeout::INFINITE)
        .map_err(|_| Failed)
}
