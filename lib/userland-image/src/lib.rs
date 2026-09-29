//! Кодек `userland.img`: таблица записей с путями и байтами, план начальной композиции.
//!
//! [`decode`] разбирает образ, [`decode_plan`] - план. Разбор заимствует исходные байты
//! без аллокации. Потребитель записи сам определяет и разбирает формат её содержимого.
//! Фича `alloc` открывает кодировщики `encode` и `encode_plan`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, allow(clippy::cast_possible_truncation))]

#[cfg(any(test, feature = "alloc"))]
extern crate alloc;

#[cfg(any(test, feature = "alloc"))]
mod builder;
mod image;
mod path;
mod plan;
#[cfg(any(test, feature = "alloc"))]
mod plan_builder;

#[cfg(any(test, feature = "alloc"))]
pub use builder::{ImageEncodeError, ImageRecord, encode};
pub use image::{
    DecodedEntries, DecodedEntry, DecodedImage, ImageDecodeError, USERLAND_IMAGE_MAGIC,
    USERLAND_IMAGE_VERSION, decode,
};
pub use path::{USERLAND_ENTRY_PATH_CAPACITY, entry_name};
pub use plan::{
    BOOT_PLAN_MAGIC, BOOT_PLAN_PATH, BOOT_PLAN_VERSION, BootPlan, COMPONENT_PROGRAM_NAME,
    PlanComponent, PlanComponents, PlanDecodeError, component_name, decode_plan,
};
#[cfg(any(test, feature = "alloc"))]
pub use plan_builder::{ComponentSpec, PlanEncodeError, encode_plan};
