use core::error::Error;
use std::{rc::Rc, thread};

use application_abi::application;
use components::component_manifest;
use executor::Executor;
use log_abi::journal;
use slint::{ComponentHandle, ModelRc, VecModel};
use slint_display::{ScreenPlatform, Shown, accept_window};

use crate::{
    follow::{follow, show, subscribe},
    ui,
    view::JournalView,
};

component_manifest! {
    struct JournalStartup {
        requirements: { journal: session(journal::Protocol) },
        exports: [application::Protocol => [{ name = "journal" }]],
    }
}

pub(crate) fn main() -> Result<(), Box<dyn Error>> {
    let executor = Rc::new(Executor::new()?);
    executor.block_on(run(&executor))
}

async fn run(executor: &Rc<Executor>) -> Result<(), Box<dyn Error>> {
    let JournalStartup { journal, parent } = components::requirements::<JournalStartup>()?;
    let Shown { window, input } = accept_window(executor, parent).await?;
    let mut platform = ScreenPlatform::open(window, executor.reactor()).await?;

    let window = ui::Journal::new().map_err(|error| format!("ui refused: {error:?}"))?;
    let rows = Rc::new(VecModel::<ui::Row>::default());
    window.set_rows(ModelRc::from(rows.clone()));
    window
        .show()
        .map_err(|error| format!("window refused: {error:?}"))?;
    platform.deliver_input(executor, input, &window);

    let columns = usize::try_from(window.get_columns())?;
    let mut view = JournalView::new(columns);
    window.set_source_column(i32::try_from(JournalView::source_column())?);
    window.set_text_column(i32::try_from(view.text_column())?);

    match subscribe(&journal, &executor.reactor()).await {
        Ok(follower) => {
            let weak = window.as_weak();
            thread::spawn(move || follow(follower, view, &weak));
        }

        Err(reason) => {
            println!("{reason}");
            show(&rows, vec![view.push_failure(&reason)]);
        }
    }

    match platform.run().await? {}
}
