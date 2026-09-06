//! Биндинги контроллеров прерываний платформы aarch64.
//!
//! [`PLATFORM_BINDINGS`] называет совместимые строки GIC и способ разбора его спецификатора
//! прерывания. Вызывающий передаёт этот набор в `DeviceTreePlatform` крейта
//! `device-resolver`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, allow(clippy::unwrap_in_result))]
#![allow(clippy::cast_possible_truncation)]

mod gic;

pub use gic::PLATFORM_BINDINGS;
