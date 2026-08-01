//! Контракты файловой подсистемы: `Namespace` разбирает путь в монтирование, `FileSystem`
//! работает с узлами внутри монтирования, `NamespaceAdmin` и `FileSystemAdmin` поднимают эти
//! процессы, а второй ещё и заводит сессии. Здесь же типы ответов, флаги открытия, правила пути
//! и раскладка записи каталога в буфере данных.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod admin;
mod cursor;
mod descriptor;
mod entry;
mod error;
mod flags;
mod kind;
mod limits;
mod mode;
mod namespace;
mod path;
mod protocol;
mod reply;

pub use admin::*;
pub use cursor::DirCursor;
pub use descriptor::NodeDescriptor;
pub use entry::{DecodedEntry, DirEntry, ENTRY_HEADER_SIZE};
pub use error::FileSystemError;
pub use flags::OpenFlags;
pub use kind::NodeKind;
pub use limits::{NAME_MAX, PATH_MAX};
pub use mode::AccessMode;
pub use namespace::*;
pub use path::{Component, Path, SEPARATOR};
pub use protocol::*;
pub use reply::{Listed, Mounted, NodeInfo, Opened, Written};
