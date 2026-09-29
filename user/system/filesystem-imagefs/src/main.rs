//! Процесс файловой системы поверх userland-образа: read-only дерево, чьи узлы лежат байтами
//! региона образа. Содержимое узла выдаётся окном этого региона, без копирования.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::{convert::Infallible, panic::PanicInfo};

use bootstrap_abi::image;
use component_startup::{Requirements as _, component_manifest};
use filesystem::ImageFileSystem;
use filesystem_abi::file_system;
use filesystem_server::{Error, Result, serve};
use kernel_objects::MemoryRegion;
use runtime::{log_fmt, process_exit, start_component};
use syscall::{Handle, kind};

const LOG_TAG: &str = "imagefs";

component_manifest! {
    struct ImageFileSystemStartup {
        requirements: { image: session(image::Protocol) },
        exports: [file_system::Protocol],
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Err(error) = run(parent);
    log_fmt(format_args!("stopped: {error}"));
    process_exit(1)
}

fn run(parent: Handle<kind::Port>) -> Result<Infallible> {
    start_component(parent, LOG_TAG);
    let requirements = ImageFileSystemStartup::request(parent).map_err(|_| Error::Start)?;
    let image = requirements.image.ok_or(Error::Start)?.client();
    let region = image
        .region()
        .ok()
        .and_then(core::result::Result::ok)
        .map(MemoryRegion::from)
        .ok_or(Error::Start)?;
    serve(parent, Some(region), |backing| {
        let backing = backing.ok_or(Error::Start)?;
        Ok(ImageFileSystem::new(backing)?)
    })
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(1)
}
