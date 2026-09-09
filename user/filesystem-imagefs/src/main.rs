//! Процесс файловой системы поверх userland-образа: read-only дерево, чьи узлы лежат байтами
//! региона образа. Содержимое узла выдаётся окном этого региона, без копирования.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{ReceivedStartup, component_manifest};
use filesystem::ImageFileSystem;
use filesystem_server::{Error, Result, serve};
use kernel_objects::Port;
use runtime::{log_fmt, thread_exit};
use syscall::{Handle, kind};

const LOG_TAG: &str = "imagefs";

component_manifest! {
    struct ImageFileSystemStartup {
        exports: { service: "FileSystem" },
        establishes_sessions: true,
        slots: { image: userland },
    }
}

/// `startup` - серверная сторона startup-порта, аргумент точки входа.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle<kind::Port>) -> ! {
    match run(startup) {
        Ok(()) => thread_exit(0),
        Err(error) => {
            log_fmt(format_args!("stopped: {error}"));
            thread_exit(1)
        }
    }
}

fn run(startup: Handle<kind::Port>) -> Result<()> {
    let ReceivedStartup { log, resources } =
        ImageFileSystemStartup::receive(Port::from_handle(startup)).map_err(|_| Error::Start)?;
    serve(
        log,
        resources.startup,
        resources.service,
        Some(resources.image),
        LOG_TAG,
        |backing| {
            let backing = backing.ok_or(Error::Start)?;
            Ok(ImageFileSystem::new(backing)?)
        },
    )
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
