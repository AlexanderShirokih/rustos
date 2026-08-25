//! Процесс файловой системы в памяти: изменяемое дерево, живущее на бюджете этого процесса.
//! Байты образа ей не нужны, всё остальное делает общий цикл обслуживания.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use component_abi::component_manifest;
use filesystem::RamFileSystem;
use filesystem_server::{Error, Result, serve};
use runtime::{OwnedHandle, PortTransport, log_fmt, thread_exit};
use syscall::Handle;

const LOG_TAG: &str = "ramfs";

component_manifest! {
    struct RamFileSystemStartup {
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
    let started = receive_startup(&transport)?;
    serve(
        started.log,
        started.control,
        started.service,
        None,
        LOG_TAG,
        |_| Ok(RamFileSystem::new()),
    )
}

fn receive_startup<T: ipc::Transport>(transport: &T) -> Result<RamFileSystemStartup> {
    RamFileSystemStartup::receive(transport)?.ok_or(Error::Start)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
