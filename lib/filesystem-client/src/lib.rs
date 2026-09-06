//! Даёт программе открывать узлы файловой подсистемы по пути, читать и писать их, обходить
//! дерево каталогов. Разбор пути, сессии файловых систем и их буфер данных остаются внутри
//! библиотеки.
//!
//! Транспорт и отображения фасад берёт у [`Environment`]. Под таргетом системы окружение
//! приходит вместе с библиотекой, на host его подставляет программа.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod cache;
mod environment;
mod error;
mod filesystem;
mod listing;
mod node;
mod route;
mod session;
#[cfg(target_os = "molecular")]
mod system;

pub use environment::{Environment, SharedRegion};
pub use error::{Failure, Result};
pub use filesystem::FileSystem;
pub use listing::Listing;
pub use node::OpenNode;
#[cfg(target_os = "molecular")]
pub use system::{FileSystemEnvironment, SessionPort, SessionRegion};
