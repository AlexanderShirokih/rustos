//! Контракты файловой подсистемы, её типы ответов и правила пути.
//!
//! Модуль [`namespace`] разбирает путь клиента в монтирование. [`file_system`] работает с
//! узлами внутри одного монтирования. [`file_system_mount`] монтирует файловую систему по
//! абсолютному пути.
//!
//! Здесь же лежат флаги открытия [`OpenFlags`], отказ [`FileSystemError`], разбор пути без
//! аллокации [`Path`] и раскладка записи каталога в буфере данных [`DirEntry`].

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

mod cursor;
mod descriptor;
mod entry;
mod error;
mod flags;
mod kind;
mod limits;
mod mode;
mod mount;
mod path;
mod protocol;
mod reply;
mod router;

pub use cursor::DirCursor;
pub use descriptor::NodeDescriptor;
pub use entry::{DecodedEntry, DirEntry, ENTRY_HEADER_SIZE};
pub use error::FileSystemError;
pub use flags::OpenFlags;
pub use kind::NodeKind;
pub use limits::{NAME_MAX, PATH_MAX};
pub use mode::AccessMode;
pub use mount::*;
pub use path::{Component, Path, PathBuf, SEPARATOR, common_directory, rebase};
pub use protocol::*;
pub use reply::{Listed, NodeInfo, Opened, Written};
pub use router::*;
