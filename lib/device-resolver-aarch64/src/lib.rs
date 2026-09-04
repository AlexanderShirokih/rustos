//! Записи описания платформы aarch64: биндинги её контроллеров прерываний.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, allow(clippy::unwrap_in_result))]
#![allow(clippy::cast_possible_truncation)]

mod gic;

pub use gic::PLATFORM_BINDINGS;
