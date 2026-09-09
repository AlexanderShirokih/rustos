//! Процесс файловой системы в памяти: изменяемое дерево, живущее на бюджете этого процесса.
//! Байты образа ей не нужны, всё остальное делает общий цикл обслуживания.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{ReceivedStartup, component_manifest};
use filesystem::RamFileSystem;
use filesystem_server::{Error, Result, serve};
use kernel_objects::Port;
use runtime::{log_fmt, thread_exit};
use syscall::{Handle, kind};

const LOG_TAG: &str = "ramfs";

component_manifest! {
    struct RamFileSystemStartup {
        exports: { service: "FileSystem" },
        establishes_sessions: true,
        slots: {},
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
        RamFileSystemStartup::receive(Port::from_handle(startup)).map_err(|_| Error::Start)?;
    serve(
        log,
        resources.startup,
        resources.service,
        None,
        LOG_TAG,
        |_| Ok(RamFileSystem::new()),
    )
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
