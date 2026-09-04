//! Процесс файловой системы поверх userland-образа: read-only дерево, чьи узлы лежат байтами
//! региона образа. Содержимое узла выдаётся окном этого региона, без копирования.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_client::{ReceivedStartup, component_manifest};
use filesystem::ImageFileSystem;
use filesystem_server::{Error, Result, serve};
use kernel_objects::OwnedHandle;
use runtime::{log_fmt, thread_exit};
use syscall::Handle;

const LOG_TAG: &str = "imagefs";

component_manifest! {
    struct ImageFileSystemStartup {
        exports: "FileSystem",
        establishes_sessions: true,
        slots: { image: userland },
    }
}

/// `startup` приходит в x0 как HandleId одноразового startup-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    match run(startup) {
        Ok(()) => thread_exit(0),
        Err(error) => {
            log_fmt(format_args!("stopped: {error}"));
            thread_exit(1)
        }
    }
}

fn run(startup: Handle) -> Result<()> {
    let ReceivedStartup { log, resources } =
        ImageFileSystemStartup::receive(OwnedHandle::from_handle(startup))
            .map_err(|_| Error::Start)?;
    serve(
        log,
        resources.control,
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
