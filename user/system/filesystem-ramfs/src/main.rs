//! Процесс файловой системы в памяти: изменяемое дерево, живущее на бюджете этого процесса.
//! Байты образа ей не нужны, всё остальное делает общий цикл обслуживания.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::{convert::Infallible, panic::PanicInfo};

use component_startup::component_manifest;
use filesystem::RamFileSystem;
use filesystem_abi::file_system;
use filesystem_server::{Result, serve};
use runtime::{log_fmt, process_exit, start_component};
use syscall::{Handle, kind};

const LOG_TAG: &str = "ramfs";

component_manifest! {
    exports: [file_system::Protocol],
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Err(error) = run(parent);
    log_fmt(format_args!("stopped: {error}"));
    process_exit(1)
}

fn run(parent: Handle<kind::Port>) -> Result<Infallible> {
    start_component(parent, LOG_TAG);
    serve(parent, None, |_| Ok(RamFileSystem::new()))
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(1)
}
