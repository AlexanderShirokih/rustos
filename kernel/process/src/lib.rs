//! Укладывает образ программы в адресное пространство нового процесса и строит по этому же
//! образу план его виртуальных диапазонов.
//!
//! [`load_user_image`] принимает отображатель страниц и [`UserImage`], а укладывает сегменты
//! образа и стек процесса. [`build_user_vm_allocator`] отдаёт по образу аллокатор свободных
//! диапазонов, [`user_entry_from_image`] - точку входа вместе с её аргументом.
//!
//! К таблицам страниц крейт обращается через трейт `MemoryMapper`, который реализует
//! платформенный слой. От архитектуры сам крейт не зависит.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(
    test,
    allow(clippy::missing_asserts_for_indexing, clippy::unwrap_in_result)
)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod entry;
pub mod from_image;
pub mod image;
pub mod loader;
pub mod plan;

pub use entry::{UserBootstrapArg, UserEntry, user_entry_from_image};
pub use from_image::{UserImageFromModelError, UserImageParts, user_image_parts_from_program};
pub use image::{UserImage, UserImageError, UserSegment};
pub use loader::load_user_image;
pub use plan::build_user_vm_allocator;
