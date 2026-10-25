//! Проверяет разрыв лога ядра у читателя, который отстал больше ёмкости кольца.

use alloc::string::String;

use bootstrap_abi::bootstrap;
use kernel_objects::{LogConsumer, MemoryRegion, Signal};
use log_abi::{Entry, LogRing};
use runtime::PortTransport;

const FILLER_SOURCE: &str = "log-filler";

const FILLER_BYTES: usize = 400;

const SPARE_LINES: u64 = 16;

#[test_harness::test]
fn a_reader_behind_the_ring_capacity_gets_a_gap_with_the_overwritten_count() {
    let kernel = bootstrap::Client::new(PortTransport::new(crate::bootstrap_handle()));
    let ring = kernel.acquire_log().wait().expect("kernel log granted");
    let region = MemoryRegion::from(ring.region);
    let region_bytes = region.inspect().expect("log region inspected").size_bytes;
    let mut consumer = LogConsumer::new(LogRing {
        region,
        signal: Signal::from(ring.signal),
    })
    .expect("log region mapped");
    let mut read = None;
    while let Some(entry) = consumer.try_next() {
        if let Entry::Record { number, .. } = entry {
            read = Some(number);
        }
    }

    // Каждая запись длиннее `FILLER_BYTES`, и строк больше, чем вмещает регион.
    let filler: String = "x".repeat(FILLER_BYTES);
    for _ in 0..region_bytes / FILLER_BYTES as u64 + SPARE_LINES {
        kernel
            .log_str(FILLER_SOURCE, &filler)
            .expect("filler logged");
    }
    // Bootstrap обслуживает кадры по порядку, и ответ вызова приходит после записи строк.
    drop(MemoryRegion::from(
        kernel
            .acquire_userland_image()
            .wait()
            .expect("barrier answered"),
    ));

    let lost = match consumer.try_next() {
        Some(Entry::Gap { lost }) => lost,
        other => panic!("gap expected, got {other:?}"),
    };
    let mut expected = match consumer.try_next() {
        Some(Entry::Record { number, .. }) => number,
        other => panic!("record expected, got {other:?}"),
    };
    let next_unread = read.map_or(0, |number| number + 1);
    test_harness::check!(lost > 0);
    test_harness::check_eq!(lost, expected - next_unread);

    let mut fillers = 0;
    while let Some(entry) = consumer.try_next() {
        let Entry::Record { number, record } = entry else {
            panic!("second gap");
        };
        expected += 1;
        test_harness::check_eq!(number, expected);
        if record.source == FILLER_SOURCE {
            fillers += 1;
        }
    }
    test_harness::check!(fillers > 0);
}
