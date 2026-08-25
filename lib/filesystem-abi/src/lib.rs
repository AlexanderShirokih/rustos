//! Контракты файловой подсистемы: `namespace` разбирает путь в монтирование, `file_system`
//! работает с узлами внутри монтирования, а `namespace_admin` меняет таблицу. Здесь же типы
//! ответов, флаги открытия, правила пути и раскладка записи каталога в буфере данных.

#![cfg_attr(not(test), no_std)]

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
pub use path::{Component, Path, SEPARATOR, rebase};
pub use protocol::*;
pub use reply::{Listed, NodeInfo, Opened, Written};
pub use router::*;
