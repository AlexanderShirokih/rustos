//! Архитектурно-независимые кубики userspace-подсистемы ядра.
//!
//! Крейт собирает в одном месте всё, что описывает user-process до его
//! фактического запуска: формат образа ([`UserImage`]), загрузчик образа в
//! адресное пространство ([`load_user_image`]), параметры первого входа в
//! user-thread, per-process VM-аллокатор ([`build_user_vm_allocator`]).

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod entry;
pub mod image;
pub mod loader;
pub mod plan;

pub use entry::{UserBootstrapArg, UserEntry, user_entry_from_image};
pub use image::{UserImage, UserImageError, UserSegment};
pub use loader::load_user_image;
pub use plan::build_user_vm_allocator;
