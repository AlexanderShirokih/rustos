//! Процесс файловой системы в памяти: изменяемое дерево, живущее на бюджете этого процесса.
//! Байты образа ей не нужны, всё остальное делает общий цикл обслуживания.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::{convert::Infallible, panic::PanicInfo};

use component_startup::component_manifest;
use filesystem::RamFileSystem;
use filesystem_abi::{file_system, namespace_admin};
use filesystem_server::{Error, Result, serve};
use runtime::{log_fmt, process_exit};
use syscall::{Handle, kind};

const LOG_TAG: &str = "ramfs";

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct RamFileSystemStartup {
        requirements: { namespace: session(namespace_admin::Protocol) },
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
    let started = RamFileSystemStartup::start(parent).map_err(|_| Error::Start)?;
    serve(
        started.log,
        parent,
        started.requirements.namespace,
        None,
        LOG_TAG,
        |_| Ok(RamFileSystem::new()),
    )
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(FAILED_EXIT_CODE)
}
