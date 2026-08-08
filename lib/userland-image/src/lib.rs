//! Адаптер бинарного формата `userland.img` и записи плана загрузочной
//! композиции без копирования данных.

#![cfg_attr(not(test), no_std)]

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
    BOOT_PLAN_MAGIC, BOOT_PLAN_PATH, BOOT_PLAN_VERSION, BootPlan, COMPONENT_PROGRAM_NAME,
    OnExhausted, PlanComponent, PlanComponents, PlanDecodeError, PlanDevice, PlanEdges,
    PlanSelectors, PlanSlots, SlotSource, decode_plan,
};
#[cfg(any(test, feature = "alloc"))]
pub use plan_builder::{ComponentSpec, DeviceSpec, PlanEncodeError, SlotSpec, encode_plan};
pub use program::{DecodedProgram, ProgramDecodeError, USERLAND_PROGRAM_MAGIC};
