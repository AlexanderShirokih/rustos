//! Файловый менеджер на Slint. Программа экспортирует `Application` с именем `files`, получает
//! окно вызовом `show` и показывает смонтированные файловые системы, записи корня выбранного
//! тома, записи выбранного каталога и начало выбранного файла. Списки и начало файла
//! прокручиваются перетаскиванием, выбор клавишами прокручивает список к выбранной записи.

#![feature(molecular_ext)]

use core::error::Error;
use std::os::molecular;

use application_abi::application;
use components::{component_manifest, into_std_handle};
use filesystem_abi::namespace;
use slint::{ComponentHandle, ModelRc, VecModel};
use slint_display::{ScreenPlatform, Shown, accept_window};

mod desktop;
mod navigation;
mod scan;

#[allow(
    unsafe_code,
    unreachable_pub,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction
)]
mod ui {
    slint::include_modules!();
}

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

fn main() -> Result<(), Box<dyn Error>> {
    let requirements = components::requirements::<FilesStartup>()?;
    let namespace = requirements.namespace.open()?;
    molecular::install_filesystem_root(into_std_handle(namespace.into_handle()))
        .map_err(|_| "filesystem root is already installed")?;

    let Shown { window, input } = accept_window(requirements.parent)?;
    let mut platform = ScreenPlatform::open(window)?;

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

    platform.deliver_input(input, &files);

    match platform.run()? {}
}
