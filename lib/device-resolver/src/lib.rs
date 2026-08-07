//! Описание платформы: по селектору устройства - его линии прерываний и MMIO-окна.
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
