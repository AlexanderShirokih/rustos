//! Адаптер бинарного формата `userland.img` без копирования данных.

#![cfg_attr(not(test), no_std)]

#[cfg(any(test, feature = "alloc"))]
extern crate alloc;

#[cfg(any(test, feature = "alloc"))]
mod builder;
mod image;
mod program;

#[cfg(any(test, feature = "alloc"))]
pub use builder::{ImageEncodeError, ImageRecord, encode, encode_program};
pub use image::{
    DecodedEntries, DecodedEntry, DecodedImage, ImageDecodeError, USERLAND_IMAGE_MAGIC,
    USERLAND_IMAGE_VERSION, decode,
};
pub use program::{DecodedProgram, ProgramDecodeError, USERLAND_PROGRAM_MAGIC};
