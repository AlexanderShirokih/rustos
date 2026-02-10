//! Общие абстракции для драйверов устройств.
//!
//! Содержит трейты, типы и макросы для реализации драйверов.

#![no_std]
extern crate alloc;

pub mod driver;
pub mod early;
pub mod probe;
pub mod runtime;
pub mod tree;

#[path = "impl/mod.rs"]
mod detail;

pub use interrupts::{IrqHandler, IrqNumber, IrqRegistrationError, IrqRegistrationToken};

pub use driver::{Driver, DriverContext, DriverDescriptor, InitOps, ProbeContext};
pub use early::{EarlyDriver, EarlyDriverContext, EarlyDriverInfo, EarlyInitOps, EarlyProbeFn};
pub use probe::{MmioAddress, MmioRequest, NodeProbeExt, ProbeError, ProbeResult};
pub use runtime::{DriverInfo, ProbeFn, RuntimeDriverRegistry, RuntimeRequestApplier};
pub use tree::{DeviceNode, DeviceTreeSource, NodeProperty};
