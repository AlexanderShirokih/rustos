//! Подписчик журнала собирает из кольца подписки строки источника [`WATCHED_SOURCE`] и пишет
//! их тексты одной строкой отчёта. Строки других источников, включая собственные, пропускает.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{string::String, vec::Vec};

use component_startup::{Requirements as _, component_manifest};
use kernel_objects::{MemoryRegion, RingConsumer, Signal};
use log_abi::{JOURNAL_SLOT_PAYLOAD, JournalAssembler, JournalEntry, journal};
use runtime::{Timeout, log_line};
use syscall::{Handle, kind};

mod failed;
mod journal_fixture_protocol;

use failed::Failed;
use journal_fixture_protocol::{REPORT_SEPARATOR, WATCHED_SOURCE};

const WATCHED_LINES: usize = 2;

const READ_TIMEOUT: Timeout = Timeout::from_secs(10);

component_manifest! {
    struct JournalSubscriberStartup {
        requirements: { journal: session(journal::Protocol) },
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    // Уход сессии снимает подписку, и строки после подписки приходят, пока сессия жива.
    let journal = JournalSubscriberStartup::from_parent(parent)
        .journal
        .open()
        .map_err(|_| Failed)?
        .client();
    let subscription = journal.subscribe().map_err(|_| Failed)?;
    let ring = RingConsumer::new(
        MemoryRegion::from(subscription.region),
        JOURNAL_SLOT_PAYLOAD,
        Signal::from(subscription.signal),
    )
    .ok_or(Failed)?;

    let mut assembler = JournalAssembler::new();
    let mut frame = [0u8; JOURNAL_SLOT_PAYLOAD];
    let mut texts = Vec::new();
    while texts.len() < WATCHED_LINES {
        let len = ring.pop(&mut frame, READ_TIMEOUT).map_err(|_| Failed)?;
        let entry = assembler.push(frame.get(..len).ok_or(Failed)?);
        if let Some(JournalEntry::Line {
            source: WATCHED_SOURCE,
            text,
            ..
        }) = entry
        {
            texts.push(String::from(text));
        }
    }
    log_line(&texts.join(REPORT_SEPARATOR));
    Ok(())
}
