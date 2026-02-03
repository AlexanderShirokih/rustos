//! Общие абстракции для драйверов устройств.
//!
//! Содержит трейты, типы и макросы для реализации драйверов.

#![no_std]
extern crate alloc;

pub mod driver;
pub mod probe;

// Реэкспорт из foundation (платформо-независимые типы)
pub use foundation::{Driver, DriverContext, MmioAddress, MmioRequest, ProbeError, ProbeResult};

// DeviceTree-специфичные типы
pub use probe::{CompatibleList, CompatibleStrings, NodeProbeExt};

pub use driver::{DriverRegistry, ProbeContext};
