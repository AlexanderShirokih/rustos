//! Подписка на лог ядра и показ его записей в окне. Поток подписки читает кольцо пачками и
//! передаёт пачку окну. Следующая пачка читается после того, как окно применило предыдущую.
//! Непрочитанные записи остаются в кольце, и отставание окна отмечает разрыв кольца. Пустое
//! кольцо ожидается со сроком `KERNEL_LINE_LATENCY`, и строка ядра, которая сигнала не
//! поднимает, появляется не позже этого срока.

use std::sync::mpsc;

use component_startup::Import;
use ipc::wire::IpcError;
use kernel_objects::{LogConsumer, MemoryRegion, SessionTransport, Signal};
use log_abi::{Level, LogRing, journal};
use slint::{Model, VecModel};
use syscall::Timeout;

use crate::{
    ui,
    view::{JournalView, LineKind, Pushed, VisualLine},
};

const BATCH_ENTRIES_MAX: usize = 64;

const KERNEL_LINE_LATENCY: Timeout = Timeout::from_millis(500);

pub(crate) struct Follower {
    _session: journal::Client<SessionTransport>,
    consumer: LogConsumer,
}

struct Batch {
    lines: Vec<Pushed>,
    stopped: bool,
}

pub(crate) fn subscribe(journal: &Import<journal::Protocol>) -> Result<Follower, String> {
    let session = journal
        .open()
        .map_err(|error| format!("journal unavailable: {error}"))?
        .client();
    let ring = session
        .subscribe()
        .map_err(|error| format!("journal subscription refused: {error:?}"))?;
    let consumer = LogConsumer::new(LogRing {
        region: MemoryRegion::from(ring.region),
        signal: Signal::from(ring.signal),
    })
    .map_err(|error| format!("log region is not mappable: {error:?}"))?;

    Ok(Follower {
        _session: session,
        consumer,
    })
}

pub(crate) fn follow(
    mut follower: Follower,
    mut view: JournalView,
    window: &slint::Weak<ui::Journal>,
) {
    loop {
        let batch = next_batch(&mut follower.consumer, &mut view);
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

fn next_batch(consumer: &mut LogConsumer, view: &mut JournalView) -> Batch {
    let mut lines = Vec::new();

    while lines.len() < BATCH_ENTRIES_MAX {
        match consumer.try_next() {
            Some(entry) => lines.push(view.push(entry)),
            None if !lines.is_empty() => break,
            None => match consumer.wait(KERNEL_LINE_LATENCY) {
                // Строка ядра сигнала не поднимает, и по истечении срока кольцо проверяется заново.
                Ok(()) | Err(IpcError::Timeout) => {}
                Err(error) => {
                    let reason = format!("log ring stopped: {error:?}");
                    println!("{reason}");
                    lines.push(view.push_failure(&reason));
                    return Batch {
                        lines,
                        stopped: true,
                    };
                }
            },
        }
    }

    Batch {
        lines,
        stopped: false,
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
        level: match line.level {
            Level::Fatal => ui::RowLevel::Fatal,
            Level::Error => ui::RowLevel::Error,
            Level::Warn => ui::RowLevel::Warn,
            Level::Info => ui::RowLevel::Info,
            Level::Debug => ui::RowLevel::Debug,
        },
        kernel: line.kernel,
        time: line.time.into(),
        source: line.source.into(),
        text: line.text.into(),
        wrapped: line.wrapped,
    }
}
