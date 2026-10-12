//! Компонент читает лог ядра через подписку `Journal`. Сначала он убеждается, что вторая
//! подписка и отображение региона на запись отвергнуты, дочитывает историю и пишет
//! [`READY_LINE`]. Потом он копит строки записи [`WATCHED_SOURCE`], строку ядра о ней с
//! префиксом [`KERNEL_MARK`] и первую строку rootkeeper о ней. Накопленное уходит одной
//! строкой через [`REPORT_SEPARATOR`], когда прочитаны строка rootkeeper и строка
//! [`END_LINE`] источника [`END_SOURCE`]. Разрыв и пропуск номера отмечаются [`GAP_MARK`],
//! убывание времени отмечается [`TIME_MARK`].

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, string::String, vec::Vec};

use component_startup::{Requirements as _, component_manifest};
use kernel_objects::{LogConsumer, Mapping, MemoryRegion, Signal};
use log_abi::{Entry, LogRing, journal};
use runtime::{Timeout, log_line};
use syscall::{Handle, UserMemFlags, kind};

mod failed;
mod journal_fixture_protocol;

use failed::Failed;
use journal_fixture_protocol::{
    END_LINE, END_SOURCE, GAP_MARK, KERNEL_MARK, READY_LINE, REPORT_SEPARATOR, TIME_MARK,
    WATCHED_SOURCE,
};

const ROOTKEEPER_SOURCE: &str = "rootkeeper";

const READ_TIMEOUT: Timeout = Timeout::from_secs(10);

component_manifest! {
    struct JournalSubscriberStartup {
        requirements: { journal: session(journal::Protocol) },
    }
}

struct Report {
    parts: Vec<String>,
    last: Option<(u64, u64)>,
    stopped: bool,
    ended: bool,
}

impl Report {
    fn push(&mut self, entry: Entry<'_>) {
        let Entry::Record { number, record } = entry else {
            self.parts.push(String::from(GAP_MARK));
            self.last = None;
            return;
        };
        if let Some((last_number, last_time)) = self.last {
            if number != last_number + 1 {
                self.parts.push(String::from(GAP_MARK));
            }
            if record.time_ns < last_time {
                self.parts.push(String::from(TIME_MARK));
            }
        }
        self.last = Some((number, record.time_ns));

        let watched = format!("'{WATCHED_SOURCE}'");
        if record.source == WATCHED_SOURCE {
            self.parts.push(String::from(record.text));
        } else if record.kernel && record.text.contains(&watched) {
            let head = record.text.split(',').next().unwrap_or_default();
            self.parts.push(format!("{KERNEL_MARK}{head}"));
        } else if !self.stopped
            && record.source == ROOTKEEPER_SOURCE
            && record.text.starts_with(WATCHED_SOURCE)
        {
            self.parts.push(String::from(record.text));
            self.stopped = true;
        } else if record.source == END_SOURCE && record.text == END_LINE {
            self.ended = true;
        }
    }

    fn done(&self) -> bool {
        self.stopped && self.ended
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let session = JournalSubscriberStartup::from_parent(parent)
        .journal
        .open()
        .map_err(|_| Failed)?
        .client();
    let ring = session.subscribe().map_err(|_| Failed)?;
    let again = session.subscribe();
    if again.is_ok() {
        log_line(&format!("second subscribe: {again:?}"));
        return Err(Failed);
    }
    let region = match Mapping::full(MemoryRegion::from(ring.region), UserMemFlags::ReadWrite) {
        Ok(_) => {
            log_line("log region mapped for writing");
            return Err(Failed);
        }
        Err(refused) => refused.region,
    };
    let mut consumer = LogConsumer::new(LogRing {
        region,
        signal: Signal::from(ring.signal),
    })
    .map_err(|_| Failed)?;

    let mut last = None;
    while let Some(entry) = consumer.try_next() {
        if let Entry::Record { number, record } = entry {
            last = Some((number, record.time_ns));
        }
    }
    log_line(READY_LINE);

    let mut report = Report {
        parts: Vec::new(),
        last,
        stopped: false,
        ended: false,
    };
    while !report.done() {
        match consumer.try_next() {
            Some(entry) => report.push(entry),
            None => consumer.wait(READ_TIMEOUT).map_err(|_| Failed)?,
        }
    }
    log_line(&report.parts.join(REPORT_SEPARATOR));
    Ok(())
}
