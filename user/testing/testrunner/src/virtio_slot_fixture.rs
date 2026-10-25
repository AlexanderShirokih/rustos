//! Клиент слота `VirtioSlot`: пишет в Status устройства ненулевое значение и завершается.
//! Стартовым handle фикстуре должны передать сессию слота.

#![no_std]
#![no_main]

use kernel_objects::Port;
use runtime::PortTransport;
use virtio::register;
use virtio_mmio_abi::virtio_slot;

mod failed;

use failed::Failed;

const ACKNOWLEDGE: u32 = 1;

#[program_entry::process]
fn acknowledge(slot: &Port) -> Result<(), Failed> {
    let client = virtio_slot::Client::new(PortTransport::new(slot));
    let status = u16::try_from(register::STATUS).map_err(|_| Failed)?;

    client.write(status, ACKNOWLEDGE).wait().map_err(|_| Failed)
}
