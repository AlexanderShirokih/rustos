//! Процесс файловой системы поверх userland-образа: read-only дерево, чьи узлы лежат байтами
//! региона образа. Содержимое узла выдаётся окном этого региона, без копирования.

#![no_std]
#![no_main]

use bootstrap_abi::image;
use component_startup::{Requirements as _, component_manifest};
use filesystem::ImageFileSystem;
use filesystem_abi::file_system;
use filesystem_server::{Error, Result, serve};
use kernel_objects::MemoryRegion;
use syscall::{Handle, kind};

component_manifest! {
    struct ImageFileSystemStartup {
        requirements: { image: session(image::Protocol) },
        exports: [file_system::Protocol => [{ fs = "image" }]],
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<()> {
    let image = ImageFileSystemStartup::from_parent(parent)
        .image
        .open()
        .map_err(|_| Error::Start)?
        .client();
    let region = image
        .region()
        .wait()
        .ok()
        .map(MemoryRegion::from)
        .ok_or(Error::Start)?;

    let Err(error) = serve(parent, Some(region), |backing| {
        let backing = backing.ok_or(Error::Start)?;
        Ok(ImageFileSystem::new(backing)?)
    });
    Err(error)
}
