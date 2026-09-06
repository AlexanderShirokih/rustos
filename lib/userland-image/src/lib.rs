//! Читает и пишет формат `userland.img`: таблицу записей образа, блоб программы и запись
//! плана загрузочной композиции.
//!
//! [`decode`] разбирает образ, [`decode_program`] - блоб одной программы, [`decode_plan`] -
//! план композиции. Разбор идёт без аллокации, разобранное ссылается прямо в исходные байты.
//!
//! Под фичей `alloc` открываются кодировщики `encode`, `encode_program` и `encode_plan`,
//! которыми сборка на host собирает образ.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, allow(clippy::cast_possible_truncation))]

#[cfg(any(test, feature = "alloc"))]
extern crate alloc;

#[cfg(any(test, feature = "alloc"))]
mod builder;
mod image;
mod plan;
#[cfg(any(test, feature = "alloc"))]
mod plan_builder;
mod program;

#[cfg(any(test, feature = "alloc"))]
pub use builder::{ImageEncodeError, ImageRecord, encode, encode_program};
pub use image::{
    DecodedEntries, DecodedEntry, DecodedImage, ImageDecodeError, USERLAND_IMAGE_MAGIC,
    USERLAND_IMAGE_VERSION, decode,
};
pub use plan::{
    BOOT_PLAN_MAGIC, BOOT_PLAN_PATH, BOOT_PLAN_VERSION, BOOT_STAGE_ONE_DIRECTORY, BootPlan,
    COMPONENT_PROGRAM_NAME, OnExhausted, PlanComponent, PlanComponents, PlanDecodeError,
    PlanDevice, PlanNames, PlanSlots, SlotSource, component_name, decode_plan,
};
#[cfg(any(test, feature = "alloc"))]
pub use plan_builder::{ComponentSpec, DeviceSpec, PlanEncodeError, SlotSpec, encode_plan};
pub use program::{DecodedProgram, ProgramDecodeError, USERLAND_PROGRAM_MAGIC, decode_program};
