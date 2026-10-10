//! Процесс файловой системы в памяти: изменяемые деревья, живущие на бюджете этого процесса.
//! Заявки с одинаковым набором атрибутов получают одно дерево, заявки с разными наборами получают
//! разные деревья. Байты образа ей не нужны, всё остальное делает общий цикл обслуживания.

#![no_std]
#![no_main]

use component_startup::component_manifest;
use filesystem::RamFileSystem;
use filesystem_abi::file_system;
use filesystem_server::{Result, serve_volumes};
use syscall::{Handle, kind};

component_manifest! {
    exports: [file_system::Protocol => [{ fs = "ram" }]],
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<()> {
    let Err(error) = serve_volumes(parent, |_| Ok(RamFileSystem::new()));
    Err(error)
}
