//! Программа-фикстура отдельного дерева в памяти: то же обслуживание, что и у общей точки
//! монтирования, но на собственном адресном пространстве. Сценарии, доводящие сервер до
//! исчерпания, работают с ней и не задевают остальные тесты.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_abi::component_manifest;
use filesystem::RamFileSystem;
use filesystem_server::{Error, Result, serve};
use kernel_objects::OwnedHandle;
use runtime::{PortTransport, log_fmt, thread_exit};
use syscall::Handle;

const LOG_TAG: &str = "sparefs";

component_manifest! {
    struct SpareFileSystemStartup {
        exports: "FileSystem",
        establishes_sessions: true,
        slots: {},
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
    let startup = OwnedHandle::from_handle(startup);
    let transport = PortTransport::server(startup.borrow());
    let started = SpareFileSystemStartup::receive(&transport)?.ok_or(Error::Start)?;
    serve(
        started.log,
        started.control,
        started.service,
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
