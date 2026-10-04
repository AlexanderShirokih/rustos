//! Отвечает по идентичности устройства, какие у него линии прерываний и MMIO-окна.
//!
//! Описание платформы объявлено трейтом [`PlatformDescription`]. Реализацию поверх device tree
//! даёт [`DeviceTreePlatform`]. Биндинги контроллеров прерываний [`ControllerBindings`] эта
//! реализация берёт из реестра. Реестр контроллеров, которые разбирает система, -
//! [`PLATFORM_BINDINGS`]. Отказ поиска назван [`ResolveError`].
#![cfg_attr(not(test), no_std)]

mod binding;
mod gic;
mod platform;
mod tree;

pub use binding::{ControllerBinding, ControllerBindings, InterruptSpecifier, SpecifierDecoder};
pub use gic::PLATFORM_BINDINGS;
pub use platform::{
    ControllerId, DeviceInterrupt, DeviceMmio, InterruptLine, PlatformDescription, ResolveError,
};
pub use tree::DeviceTreePlatform;
