#![no_std]
#![no_main]

use filesystem_abi::{AccessMode, IO_FRAME_BYTES, IoPhase, IoRecord, OpenFlags, file_system};
use ipc::{Timeout, wire::MessageBuf};
use kernel_objects::{MemoryRegion, Port, RingConsumer, RingProducer, Signal};
use runtime::PortTransport;

mod failed;

use failed::Failed;

const PATH: &str = "/fixture";

#[program_entry::process]
fn populate_session(service: &Port) -> Result<(), Failed> {
    let client = file_system::Client::new(PortTransport::new(service));

    let exchange = client.start_io().wait().map_err(|_| Failed)?;
    let requests = RingProducer::new(
        MemoryRegion::from(exchange.requests),
        IO_FRAME_BYTES,
        Signal::from(exchange.request_ready),
    )
    .ok_or(Failed)?;
    let responses = RingConsumer::new(
        MemoryRegion::from(exchange.responses),
        IO_FRAME_BYTES,
        Signal::from(exchange.response_ready),
    )
    .ok_or(Failed)?;

    let path = PATH;
    let opened = client
        .open(path, AccessMode::ReadWrite, OpenFlags::CREATE)
        .wait()
        .map_err(|_| Failed)?;
    let mut message = MessageBuf::new();
    IoRecord::request(1, IoPhase::Write, opened.fd, 0, 4)
        .encode(&mut message)
        .map_err(|_| Failed)?;
    requests.push(message.as_bytes()).map_err(|_| Failed)?;
    let mut bytes = [0; IO_FRAME_BYTES];
    let len = responses
        .pop(&mut bytes, Timeout::from_secs(2))
        .map_err(|_| Failed)?;
    let response = IoRecord::decode(bytes.get(..len).ok_or(Failed)?).map_err(|_| Failed)?;
    if response.id != 1 || response.phase != IoPhase::Register || response.result != Ok(4) {
        return Err(Failed);
    }
    // Выход оставляет активный Lease серверу, чтобы родитель проверил отмену при SenderGone.
    Ok(())
}
