//! Архитектурно-независимые типы user-process: образ, загрузчик, VM-аллокатор.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod entry;
pub mod from_image;
pub mod image;
pub mod loader;
pub mod plan;

pub use entry::{UserBootstrapArg, UserEntry, user_entry_from_image};
pub use from_image::{UserImageFromAbiError, UserImageParts, user_image_parts_from_entry};
pub use image::{UserImage, UserImageError, UserSegment};
pub use loader::load_user_image;
pub use plan::build_user_vm_allocator;
