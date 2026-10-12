//! Строки лога в консоли и в кольце: время, ранние записи, признак ядра.
//!
//! Файл держит единственный тест. Сценарий зависит от нетронутых статиков `klog`, которые
//! свежи только в собственном тестовом бинаре.

mod common;

use core::sync::atomic::AtomicU64;
use std::{boxed::Box, vec::Vec};

use common::Recorder;
use log_abi::{Entry, Level, RecordBuf, RingReader, ring_words};

#[test]
fn lines_reach_the_console_and_the_ring_in_order() {
    let console: &'static Recorder = Box::leak(Box::default());
    klog::set_stdout(console);
    klog::info!("early.rs"; "before the ring");

    let words: &'static [AtomicU64] =
        Vec::leak((0..ring_words(4096)).map(|_| AtomicU64::new(0)).collect());
    klog::attach_ring(words);
    klog::set_clock(Box::new(|| 1_500_000_000));
    klog::debug!("gic.rs"; "irq {} bound", 33);
    klog::log_user("rootkeeper", "composition started");

    assert_eq!(
        console.normal_contents(),
        "[    0.000] [I] [early.rs] before the ring\r\n\
         [    1.500] [D] [gic.rs] irq 33 bound\r\n\
         [    1.500] [I] [rootkeeper] composition started\r\n"
    );
    let mut reader = RingReader::new();
    let mut buf = RecordBuf::new();
    let mut read = Vec::new();
    while let Some(Entry::Record { number, record }) = reader.next(words, &mut buf) {
        read.push((
            number,
            record.time_ns,
            record.level,
            record.kernel,
            String::from(record.source),
            String::from(record.text),
        ));
    }
    assert_eq!(
        read,
        [
            (
                0,
                0,
                Level::Info,
                true,
                "early.rs".into(),
                "before the ring".into()
            ),
            (
                1,
                1_500_000_000,
                Level::Debug,
                true,
                "gic.rs".into(),
                "irq 33 bound".into()
            ),
            (
                2,
                1_500_000_000,
                Level::Info,
                false,
                "rootkeeper".into(),
                "composition started".into()
            ),
        ]
    );
}
