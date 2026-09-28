//! Отвечает по селектору устройства, какие у него линии прерываний и MMIO-окна.
//!
//! Описание платформы объявлено трейтом [`PlatformDescription`]. Реализацию поверх device tree
//! даёт [`DeviceTreePlatform`]. Биндинги контроллеров прерываний [`ControllerBindings`] эта
//! реализация берёт у крейта архитектуры. Отказ поиска назван [`ResolveError`].
//!
//! [`acquire_mmio`] берёт у ядра capability на окно памяти, которое содержит найденный
//! [`DeviceMmio`], и смещение устройства в этом окне.
#![cfg_attr(not(test), no_std)]

mod binding;
mod platform;
mod tree;
mod vend;

pub use binding::{ControllerBinding, ControllerBindings, InterruptSpecifier, SpecifierDecoder};
pub use platform::{
    ControllerId, DeviceInterrupt, DeviceMmio, DeviceSelector, InterruptLine, PlatformDescription,
    ResolveError,
};
pub use tree::DeviceTreePlatform;
pub use vend::acquire_mmio;
