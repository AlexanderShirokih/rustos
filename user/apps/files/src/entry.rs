use core::error::Error;
use std::{os::molecular, rc::Rc};

use application_abi::application;
use components::{component_manifest, into_std_handle};
use executor::Executor;
use filesystem_abi::namespace;
use slint::{ComponentHandle, ModelRc, VecModel};
use slint_display::{ScreenPlatform, Shown, receive_window};

use crate::{desktop, ui};

const NARROW_WIDTH_MAX: f32 = 600.0;

const MOUNTS: [(&str, &str); 2] = [("/boot", "image, read-only"), ("/tmp", "ramfs")];

component_manifest! {
    struct FilesStartup {
        requirements: {
            namespace: session(namespace::Protocol),
        },
        exports: [application::Protocol => [{ name = "files" }]],
    }
}

pub(crate) fn main() -> Result<(), Box<dyn Error>> {
    let executor = Rc::new(Executor::new()?);
    executor.block_on(run(&executor))
}

async fn run(executor: &Rc<Executor>) -> Result<(), Box<dyn Error>> {
    let requirements = components::requirements::<FilesStartup>()?;
    let namespace = requirements
        .namespace
        .open_async(&executor.reactor())
        .await?;
    molecular::install_filesystem_root(into_std_handle(namespace.into_handle()))
        .map_err(|_| "filesystem root is already installed")?;

    let Shown { window, input } = receive_window(executor, requirements.parent).await?;
    let mut platform = ScreenPlatform::open(window, executor.reactor()).await?;

    let files = ui::Files::new().map_err(|error| format!("ui refused: {error:?}"))?;
    files.set_narrow(platform.logical_size().width < NARROW_WIDTH_MAX);
    files
        .global::<ui::Desktop<'_>>()
        .set_places(ModelRc::new(VecModel::from(
            MOUNTS
                .iter()
                .map(|(path, note)| ui::Place {
                    path: (*path).into(),
                    note: (*note).into(),
                })
                .collect::<Vec<_>>(),
        )));
    desktop::start_browsing(&files, MOUNTS.iter().map(|(path, _)| *path).collect());
    files
        .show()
        .map_err(|error| format!("window refused: {error:?}"))?;

    platform.deliver_input(executor, input, &files);

    match platform.run().await? {}
}
