//! Модель файловой системы: трейт бекенда, референсный ramfs, дерево поверх userland-образа и
//! таблица сессий. Модель независима от системных вызовов и IPC.
//!
//! Модель обслуживает одно монтирование. Корень её дерева имён есть корень монтирования, и
//! пути отсчитываются от него. Ни префикс этого монтирования, ни соседние монтирования модели
//! не переданы, соответствие префиксов монтированиям держит маршрутизатор имён.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(
    test,
    allow(clippy::cast_possible_truncation, clippy::unwrap_in_result)
)]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod backend;
mod image;
mod node;
mod ram;
mod session;
mod store;

pub use backend::{FileSystemBackend, NodeMetadata};
pub use image::{ImageFileSystem, ImageFileSystemError};
pub use node::{NodeId, Window};
pub use ram::RamFileSystem;
pub use session::{ClientId, OpenNode, Session, SessionTable};
pub use store::{ByteStore, HeapByteStore, StoreId, insert_store};
