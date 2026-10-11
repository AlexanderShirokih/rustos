//! Файловый менеджер на Slint. Показывает смонтированные файловые системы, записи корня
//! выбранного тома, записи выбранного каталога и начало выбранного файла.

#![feature(molecular_ext)]

use core::error::Error;
use std::{os::molecular, thread};

use components::{component_manifest, into_std_handle};
use display_abi::display;
use filesystem_abi::namespace;
use input_abi::input;
use ipc::Timeout;
use slint::{ComponentHandle, ModelRc, VecModel};
use slint_display::{ScreenPlatform, scale_factor};

mod desktop;
mod events;
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

const MOUNTS: [(&str, &str); 2] = [("/", "image, read-only"), ("/tmp", "ramfs")];

component_manifest! {
    struct FilesStartup {
        requirements: {
            screen: session(display::Protocol),
            namespace: session(namespace::Protocol),
            input: session(input::Protocol),
        },
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let requirements = components::requirements::<FilesStartup>()?;
    let screen = requirements.screen.open()?.client();
    let namespace = requirements.namespace.open()?;
    let input = requirements
        .input
        .open()?
        .client()
        .with_wait(Timeout::INFINITE);
    molecular::install_filesystem_root(into_std_handle(namespace.into_handle()))
        .map_err(|_| "filesystem root is already installed")?;

    let mut platform = ScreenPlatform::open(screen)?;
    let dimension = platform.dimension();
    let scale = scale_factor(dimension, platform.physical_size());
    platform.set_scale_factor(scale);
    let width = f32::from(u16::try_from(dimension.width).unwrap_or(u16::MAX));
    let height = f32::from(u16::try_from(dimension.height).unwrap_or(u16::MAX));

    let files = ui::Files::new().map_err(|error| format!("ui refused: {error:?}"))?;
    files.set_narrow(width / scale < NARROW_WIDTH_MAX);
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
    println!(
        "files on {}x{}, scale {scale:.2}",
        dimension.width, dimension.height
    );

    let weak = files.as_weak();
    let bounds = events::Screen {
        width,
        height,
        scale,
    };
    thread::spawn(move || events::deliver(&input, bounds, &weak));

    Err(platform.run(|_| {}).into())
}
