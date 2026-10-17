//! Просмотр системного журнала на экране. Программа экспортирует `Application` с именем
//! `journal`, получает окно и ввод вызовом `show` и показывает последние записи лога ядра на
//! всей поверхности окна по сетке моноширинного шрифта. Строки прокручиваются перетаскиванием,
//! и пока показана последняя строка, журнал следит за концом. Отказ подписки программа
//! показывает строкой с причиной, отказ окна её завершает.

use core::error::Error;
use std::{rc::Rc, thread};

use application_abi::application;
use components::component_manifest;
use follow::{follow, show, subscribe};
use log_abi::journal;
use slint::{ComponentHandle, ModelRc, VecModel};
use slint_display::{ScreenPlatform, Shown, accept_window};
use view::JournalView;

mod follow;
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

component_manifest! {
    struct JournalStartup {
        requirements: { journal: session(journal::Protocol) },
        exports: [application::Protocol => [{ name = "journal" }]],
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let JournalStartup { journal, parent } = components::requirements::<JournalStartup>()?;
    let Shown { window, input } = accept_window(parent)?;
    let mut platform = ScreenPlatform::open(window)?;

    let window = ui::Journal::new().map_err(|error| format!("ui refused: {error:?}"))?;
    let rows = Rc::new(VecModel::<ui::Row>::default());
    window.set_rows(ModelRc::from(rows.clone()));
    window
        .show()
        .map_err(|error| format!("window refused: {error:?}"))?;
    platform.deliver_input(input, &window);

    let columns = usize::try_from(window.get_columns())?;
    let mut view = JournalView::new(columns);
    window.set_source_column(i32::try_from(JournalView::source_column())?);
    window.set_text_column(i32::try_from(view.text_column())?);

    match subscribe(&journal) {
        Ok(follower) => {
            let weak = window.as_weak();
            thread::spawn(move || follow(follower, view, &weak));
        }
        Err(reason) => {
            println!("{reason}");
            show(&rows, vec![view.push_failure(&reason)]);
        }
    }

    match platform.run()? {}
}
