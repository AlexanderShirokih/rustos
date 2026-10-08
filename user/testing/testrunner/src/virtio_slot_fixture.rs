//! Клиент слота `VirtioSlot`: пишет в Status устройства ненулевое значение и завершается.
//! Стартовым handle фикстуре должны передать сессию слота.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

use kernel_objects::Port;
use runtime::PortTransport;
use syscall::Timeout;
use virtio::register;
use virtio_mmio_abi::virtio_slot;

mod fixture_start;

use fixture_start::{Failed, fixture_start};

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const ACKNOWLEDGE: u32 = 1;

fixture_start!(acknowledge);

fn acknowledge(slot: &Port) -> Result<(), Failed> {
    let client = virtio_slot::Client::new(PortTransport::new(slot)).with_wait(CALL_TIMEOUT);
    let status = u16::try_from(register::STATUS).map_err(|_| Failed)?;

    client.write(status, ACKNOWLEDGE).map_err(|_| Failed)
}
