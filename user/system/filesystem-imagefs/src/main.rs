//! Процесс файловой системы поверх userland-образа: read-only дерево, чьи узлы лежат байтами
//! региона образа. Содержимое узла выдаётся окном этого региона, без копирования.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::{convert::Infallible, panic::PanicInfo};

use component_startup::component_manifest;
use filesystem::ImageFileSystem;
use filesystem_abi::{file_system, namespace_admin};
use filesystem_server::{Error, Result, serve};
use runtime::{log_fmt, process_exit};
use syscall::{Handle, kind};

const LOG_TAG: &str = "imagefs";

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct ImageFileSystemStartup {
        requirements: { namespace: session(namespace_admin::Protocol), image: region },
        exports: [file_system::Protocol],
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Err(error) = run(parent);
    log_fmt(format_args!("stopped: {error}"));
    process_exit(FAILED_EXIT_CODE)
}

fn run(parent: Handle<kind::Port>) -> Result<Infallible> {
    let started = ImageFileSystemStartup::start(parent).map_err(|_| Error::Start)?;
    serve(
        started.log,
        parent,
        started.requirements.namespace,
        Some(started.requirements.image),
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
    process_exit(FAILED_EXIT_CODE)
}
