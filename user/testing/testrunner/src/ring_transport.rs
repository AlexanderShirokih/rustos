//! E2E проверка `RingProducer`/`RingConsumer` из EL0: концы над общим
//! регионом-кольцом (`SpscRing`) в shared memory и одним "data"-сигналом.
//!
//! Главный поток выступает стороной producer, рабочий поток - стороной
//! consumer. Регион создаётся через `Resource`/`MemoryRegion`, consumer получает
//! копии региона и сигнала, и каждый конец отображает регион у себя. Consumer
//! вызовами `pop` выгребает `FRAMES` кадров, producer шлёт те же `FRAMES`
//! (повтор на `Full`). Проверяем, что все кадры получены по порядку.

use io::RingError;
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
            Err(RingError::Full) => core::hint::spin_loop(),
            Err(_) => test_harness::check!(false),
        }
    }

    thread
        .wait(JOIN_TIMEOUT)
        .expect("consumer finishes before timeout");
    test_harness::check_eq!(thread.exit_code().expect("consumer exit code"), 0);
    thread.join(JOIN_TIMEOUT).expect("consumer joins");
}
