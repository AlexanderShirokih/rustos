//! E2E проверка `RingProducer`/`RingConsumer` из EL0: концы над общим
//! регионом-кольцом (`SpscRing`) в shared memory и одним "data"-сигналом.
//!
//! Главный поток выступает стороной producer, рабочий поток - стороной
//! consumer. Регион создаётся через `Resource`/`MemoryRegion`, consumer получает
//! копии региона и сигнала, и каждый конец отображает регион у себя. Consumer
//! вызовами `pop` выгребает `FRAMES` кадров, producer шлёт те же `FRAMES`
//! (повтор на `Full`). Проверяем, что все кадры получены по порядку.

use ipc::wire::IpcError;
use kernel_objects::{Resource, RingConsumer, RingProducer, Signal};
use runtime::{MemoryAccess, Timeout, spawn_owned};
use syscall::Rights;

const SLOT_PAYLOAD: usize = 4;
const FRAMES: u32 = 4096;

const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const WAIT_TIMEOUT: Timeout = Timeout::from_secs(1);

const PAGE_SIZE: u64 = syscall::PAGE_SIZE as u64;

fn drain(consumer: &RingConsumer) -> u32 {
    for next in 0..FRAMES {
        let mut buf = [0u8; SLOT_PAYLOAD];
        match consumer.pop(&mut buf, WAIT_TIMEOUT) {
            Ok(SLOT_PAYLOAD) if u32::from_le_bytes(buf) == next => {}
            _ => return 1,
        }
    }
    0
}

#[test_harness::test]
fn ring_transport_round_trip() {
    let resource = Resource::self_resource().expect("self metering resource");
    let region = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("region");
    let consumer_region = region
        .duplicate(Rights::READ | Rights::WRITE, None)
        .expect("duplicate region");

    let data_signal = Signal::create().expect("signal create");
    let consumer_signal = data_signal
        .duplicate(Rights::READ | Rights::WRITE, None)
        .expect("duplicate data signal");

    let consumer =
        RingConsumer::new(consumer_region, SLOT_PAYLOAD, consumer_signal).expect("consumer ring");
    let producer = RingProducer::new(region, SLOT_PAYLOAD, data_signal).expect("producer ring");

    let thread = spawn_owned(move || drain(&consumer)).expect("spawn consumer worker");

    let mut sent = 0u32;
    while sent < FRAMES {
        match producer.push(&sent.to_le_bytes()) {
            Ok(()) => sent += 1,
            Err(IpcError::ResourceExhausted) => core::hint::spin_loop(),
            Err(_) => test_harness::check!(false),
        }
    }

    thread
        .wait(JOIN_TIMEOUT)
        .expect("consumer finishes before timeout");
    test_harness::check_eq!(thread.exit_code().expect("consumer exit code"), 0);
    thread.join(JOIN_TIMEOUT).expect("consumer joins");
}

#[test_harness::test]
fn ring_transport_reports_failed_notification_after_publication() {
    let resource = Resource::self_resource().expect("resource");
    let region = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("region");
    let other = region
        .duplicate(Rights::READ | Rights::WRITE, None)
        .expect("receiver region");
    let signal = Signal::create().expect("signal");
    let read_only = signal
        .duplicate(Rights::READ, None)
        .expect("read-only signal");
    let consumer = RingConsumer::new(other, SLOT_PAYLOAD, signal).expect("consumer");
    let producer = RingProducer::new(region, SLOT_PAYLOAD, read_only).expect("producer");
    test_harness::check_eq!(
        producer.push(&42u32.to_le_bytes()),
        Err(IpcError::PeerClosed)
    );
    let mut bytes = [0; SLOT_PAYLOAD];
    test_harness::check_eq!(consumer.try_pop(&mut bytes), Ok(Some(SLOT_PAYLOAD)));
    test_harness::check_eq!(u32::from_le_bytes(bytes), 42);
    test_harness::check_eq!(consumer.try_pop(&mut bytes), Ok(None));
}
