//! Фасад файловой подсистемы для программ userspace. Путь превращается в сессию файловой
//! системы, дескриптор несёт свою позицию, а обход дерева и рекурсивное удаление идут поверх
//! тех же сессий.
//!
//! Крейт параметризован окружением вызывающего ([`Environment`]) и системных вызовов не делает.
//! Capability он отдаёт наружу, а транспорт и отображение региона получает от окружения.

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

pub use environment::{Environment, SharedRegion};
pub use error::{Failure, Result};
pub use filesystem::FileSystem;
pub use listing::Listing;
pub use node::OpenNode;
