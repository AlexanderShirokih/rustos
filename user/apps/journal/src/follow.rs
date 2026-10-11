//! Подписка на кольцо журнала и показ его строк в окне. Поток подписки читает кольцо пачками и
//! передаёт пачку окну. Следующая пачка читается после того, как окно применило предыдущую.
//! Непрочитанные строки остаются в кольце, и отставание окна отмечает разрыв кольца.

use std::sync::mpsc;

use component_startup::Import;
use ipc::wire::IpcError;
use kernel_objects::{MemoryRegion, RingConsumer, SessionTransport, Signal};
use log_abi::{JOURNAL_SLOT_PAYLOAD, JournalAssembler, journal};
use slint::{Model, VecModel};
use syscall::Timeout;

use crate::{
    ui,
    view::{JournalView, LineKind, Pushed, VisualLine},
};

const BATCH_ENTRIES_MAX: usize = 64;

pub(crate) struct Follower {
    _session: journal::Client<SessionTransport>,
    ring: RingConsumer,
}

struct Batch {
    lines: Vec<Pushed>,
    stopped: bool,
}

struct BatchReader<'r> {
    ring: &'r RingConsumer,
    assembler: JournalAssembler,
    frame: [u8; JOURNAL_SLOT_PAYLOAD],
}

impl<'r> BatchReader<'r> {
    fn new(ring: &'r RingConsumer) -> Self {
        Self {
            ring,
            assembler: JournalAssembler::new(),
            frame: [0; JOURNAL_SLOT_PAYLOAD],
        }
    }

    fn next(&mut self, view: &mut JournalView) -> Batch {
        let mut lines = Vec::new();
        let mut wait = Timeout::INFINITE;

        while lines.len() < BATCH_ENTRIES_MAX {
            match self.ring.pop(&mut self.frame, wait) {
                Ok(len) => {
                    let entry = self
                        .frame
                        .get(..len)
                        .and_then(|frame| self.assembler.push(frame));
                    if let Some(entry) = entry {
                        lines.push(view.push(entry));
                    }
                }
                Err(IpcError::Timeout) => break,
                Err(error) => {
                    let reason = format!("journal ring stopped: {error:?}");
                    println!("{reason}");
                    lines.push(view.push_failure(&reason));
                    return Batch {
                        lines,
                        stopped: true,
                    };
                }
            }

            // Первый кадр пачки ждёт без срока, остальные забираются из кольца без ожидания.
            wait = Timeout::POLL;
        }

        Batch {
            lines,
            stopped: false,
        }
    }
}

pub(crate) fn subscribe(journal: &Import<journal::Protocol>) -> Result<Follower, String> {
    let session = journal
        .open()
        .map_err(|error| format!("journal unavailable: {error}"))?
        .client();
    let subscription = session
        .subscribe()
        .map_err(|error| format!("journal subscription refused: {error:?}"))?;
    let ring = RingConsumer::new(
        MemoryRegion::from(subscription.region),
        JOURNAL_SLOT_PAYLOAD,
        Signal::from(subscription.signal),
    )
    .ok_or("journal ring is not mappable")?;

    Ok(Follower {
        _session: session,
        ring,
    })
}

pub(crate) fn follow(
    follower: &Follower,
    mut view: JournalView,
    window: &slint::Weak<ui::Journal>,
) {
    let mut reader = BatchReader::new(&follower.ring);

    loop {
        let batch = reader.next(&mut view);
        if !apply(window, batch.lines) || batch.stopped {
            return;
        }
    }
}

pub(crate) fn show(rows: &VecModel<ui::Row>, lines: Vec<Pushed>) {
    for pushed in lines {
        for _ in 0..pushed.evicted.min(rows.row_count()) {
            rows.remove(0);
        }
        rows.extend(pushed.lines.into_iter().map(row));
    }
}

fn apply(window: &slint::Weak<ui::Journal>, lines: Vec<Pushed>) -> bool {
    let (applied, wait_applied) = mpsc::sync_channel(1);
    let posted = window.upgrade_in_event_loop(move |window| {
        if let Some(rows) = window.get_rows().as_any().downcast_ref() {
            show(rows, lines);
        }
        let _ = applied.send(());
    });

    posted.is_ok() && wait_applied.recv().is_ok()
}

fn row(line: VisualLine) -> ui::Row {
    ui::Row {
        kind: match line.kind {
            LineKind::Line => ui::RowKind::Line,
            LineKind::Gap => ui::RowKind::Gap,
            LineKind::Failure => ui::RowKind::Failure,
        },
        source: line.source.into(),
        text: line.text.into(),
    }
}
