//! Просмотр системного журнала на экране. Программа подписывается на журнал и показывает его
//! последние записи на всей поверхности сессии `Display` по сетке моноширинного шрифта.
//! Отказ подписки программа показывает строкой с причиной, отказ экрана её завершает.

use core::error::Error;
use std::{rc::Rc, sync::mpsc, thread};

use components::component_manifest;
use display_abi::{PhysicalSize, display};
use ipc::wire::IpcError;
use kernel_objects::{MemoryRegion, RingConsumer, SessionTransport, Signal};
use log_abi::{JOURNAL_SLOT_PAYLOAD, JournalAssembler, journal};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use slint_display::{ScreenPlatform, scale_factor};
use syscall::Timeout;
use view::{JournalView, LineKind, Pushed, VisualLine};

mod view;

#[allow(
    unreachable_pub,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::todo,
    clippy::unwrap_used
)]
mod ui {
    slint::include_modules!();
}

const BATCH_ENTRIES_MAX: usize = 64;

component_manifest! {
    struct JournalStartup {
        requirements: {
            screen: session(display::Protocol),
            journal: session(journal::Protocol),
        },
    }
}

struct Follower {
    _session: journal::Client<SessionTransport>,
    ring: RingConsumer,
}

fn main() -> Result<(), Box<dyn Error>> {
    let requirements = components::requirements::<JournalStartup>()?;
    let screen = requirements.screen.open()?.client();
    let mut platform = ScreenPlatform::open(screen)?;
    let dimension = platform.dimension();
    let physical_size = platform.physical_size();
    let scale = scale_factor(dimension, physical_size);
    platform.set_scale_factor(scale);

    let window = ui::Journal::new().map_err(|error| format!("ui refused: {error:?}"))?;
    let rows = Rc::new(VecModel::<ui::Row>::default());
    window.set_rows(ModelRc::from(rows.clone()));
    window
        .show()
        .map_err(|error| format!("window refused: {error:?}"))?;
    let columns = usize::try_from(window.get_columns())?;
    let mut view = JournalView::new(columns);
    window.set_text_column(i32::try_from(view.text_column())?);
    match physical_size {
        Some(PhysicalSize {
            width_mm,
            height_mm,
        }) => println!(
            "journal on {}x{}, {width_mm}x{height_mm} mm, scale {scale:.2}, {columns} columns",
            dimension.width, dimension.height
        ),
        None => println!(
            "journal on {}x{}, physical size unknown, scale {scale:.2}, {columns} columns",
            dimension.width, dimension.height
        ),
    }

    match subscribe(&requirements) {
        Ok(follower) => {
            let weak = window.as_weak();
            thread::spawn(move || follow(&follower, view, &weak));
        }
        Err(reason) => {
            println!("{reason}");
            show(&rows, vec![view.push_failure(&reason)]);
        }
    }
    Err(platform.run(|_| {}).into())
}

fn subscribe(requirements: &JournalStartup) -> Result<Follower, String> {
    let session = requirements
        .journal
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

fn follow(follower: &Follower, mut view: JournalView, window: &slint::Weak<ui::Journal>) {
    let mut assembler = JournalAssembler::new();
    let mut frame = [0u8; JOURNAL_SLOT_PAYLOAD];

    loop {
        // Первый кадр пачки ждёт без срока, остальные забираются из кольца без ожидания.
        let mut batch = Vec::new();
        let mut wait = Timeout::INFINITE;
        let mut stopped = false;

        while batch.len() < BATCH_ENTRIES_MAX {
            match follower.ring.pop(&mut frame, wait) {
                Ok(len) => {
                    if let Some(entry) = frame.get(..len).and_then(|frame| assembler.push(frame)) {
                        batch.push(view.push(entry));
                    }
                }
                Err(IpcError::Timeout) => break,
                Err(error) => {
                    let reason = format!("journal ring stopped: {error:?}");
                    println!("{reason}");
                    batch.push(view.push_failure(&reason));
                    stopped = true;
                    break;
                }
            }

            wait = Timeout::POLL;
        }

        // Следующая пачка читается после того, как окно применило эту. Непрочитанные строки
        // остаются в кольце, и отставание окна отмечает разрыв кольца.
        let (applied, wait_applied) = mpsc::sync_channel(1);
        let posted = window.upgrade_in_event_loop(move |window| {
            if let Some(rows) = window.get_rows().as_any().downcast_ref() {
                show(rows, batch);
            }
            let _ = applied.send(());
        });

        if stopped || posted.is_err() || wait_applied.recv().is_err() {
            return;
        }
    }
}

fn show(rows: &VecModel<ui::Row>, batch: Vec<Pushed>) {
    for pushed in batch {
        for _ in 0..pushed.evicted.min(rows.row_count()) {
            rows.remove(0);
        }
        rows.extend(pushed.lines.into_iter().map(row));
    }
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
