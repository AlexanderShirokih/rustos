//! Процесс файловой системы в памяти: изменяемое дерево, живущее на бюджете этого процесса.
//! Байты образа ей не нужны, всё остальное делает общий цикл обслуживания.

#![no_std]
#![no_main]

use component_startup::component_manifest;
use filesystem::RamFileSystem;
use filesystem_abi::file_system;
use filesystem_server::{Result, serve};
use syscall::{Handle, kind};

component_manifest! {
    exports: [file_system::Protocol],
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<()> {
    let Err(error) = serve(parent, None, |_| Ok(RamFileSystem::new()));
    Err(error)
}
