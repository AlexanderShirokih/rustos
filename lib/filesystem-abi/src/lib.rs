//! Контракты файловой подсистемы, её типы ответов и правила пути.
//!
//! Модуль [`namespace`] разбирает путь клиента в монтирование. [`file_system`] работает с
//! узлами внутри одного монтирования. [`namespace_admin`] меняет таблицу монтирований.
//!
//! Здесь же лежат флаги открытия [`OpenFlags`], отказ [`FileSystemError`], разбор пути без
//! аллокации [`Path`], раскладка записи каталога в буфере данных [`DirEntry`] и формат
//! таблицы монтирований [`MountTable`].

#![cfg_attr(not(test), no_std)]
#![cfg_attr(
    test,
    allow(
        clippy::cast_possible_truncation,
        clippy::missing_asserts_for_indexing,
        clippy::unwrap_in_result
    )
)]

extern crate alloc;
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
mod mount_table;
mod path;
mod protocol;
mod reply;
mod router;

pub use admin::*;
pub use cursor::DirCursor;
pub use descriptor::NodeDescriptor;
pub use entry::{DecodedEntry, DirEntry, ENTRY_HEADER_SIZE};
pub use error::FileSystemError;
pub use flags::OpenFlags;
pub use kind::NodeKind;
pub use limits::{NAME_MAX, PATH_MAX};
pub use mode::AccessMode;
pub use mount_table::{
    BOOT_MOUNT_TABLE_PATH, MOUNT_TABLE_MAGIC, MOUNT_TABLE_VERSION, Mount, MountTable,
    MountTableDecodeError, MountTableEncodeError, Mounts, decode_mount_table, encode_mount_table,
};
pub use path::{Component, Path, SEPARATOR, common_directory, rebase};
pub use protocol::*;
pub use reply::{Listed, NodeInfo, Opened, Written};
pub use router::*;
