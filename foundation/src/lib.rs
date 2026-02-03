//! Базовые абстракции ядра.
//!
//! Содержит платформо-независимые трейты и типы, которые используются
//! как контракты между различными компонентами системы.

#![no_std]

pub mod driver;
pub mod probe;

pub use driver::{Driver, DriverContext};
pub use probe::{MmioAddress, MmioRequest, ProbeError, ProbeResult};
