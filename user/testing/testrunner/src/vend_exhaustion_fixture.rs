#![no_std]
#![no_main]

use bootstrap_abi::bootstrap;
use ipc::CallError;
use kernel_objects::{MemoryRegion, Port};
use runtime::PortTransport;
use syscall::HANDLE_TABLE_CAPACITY;

mod failed;
mod handle_table_fill;

use failed::Failed;
use handle_table_fill::fill_handle_table;

#[program_entry::process]
fn run(kernel: &Port) -> Result<(), Failed> {
    let client = bootstrap::Client::new(PortTransport::new(kernel));
    let spare = fill_handle_table().ok_or(Failed)?;

    for _ in 0..=HANDLE_TABLE_CAPACITY {
        if !matches!(
            client.acquire_userland_image().wait(),
            Err(CallError::Ipc(_))
        ) {
            return Err(Failed);
        }
    }

    drop(spare);
    let image = client.acquire_userland_image().wait().map_err(|_| Failed)?;
    drop(MemoryRegion::from(image));
    Ok(())
}
