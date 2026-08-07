//! Записи описания платформы aarch64: биндинги её контроллеров прерываний.
#![cfg_attr(not(test), no_std)]

mod gic;

pub use gic::PLATFORM_BINDINGS;
