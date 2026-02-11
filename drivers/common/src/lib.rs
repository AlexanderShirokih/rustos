//! Общие абстракции для драйверов устройств.
//!
//! Содержит трейты, типы и макросы для реализации драйверов.

#![no_std]
extern crate alloc;

pub mod driver;
pub mod early;
pub mod probe;
pub mod scanner;
pub mod tree;

mod mmio;
mod registry;

pub use interrupts::{IrqHandler, IrqNumber, IrqRegistrationError};

pub use memory::mem_flags::*;
pub use mmio::*;
pub use registry::*;

pub use driver::{Driver, DriverContext, DriverDescriptor, ProbeContext};
pub use early::{
    EarlyDriver, EarlyDriverContext, EarlyDriverInfo, EarlyInitOps, EarlyProbeFn, EarlyProbeResult,
};

pub use probe::{ProbeError, ProbeResult};
pub use scanner::{DriverInfo, ProbeFn};
pub use tree::{DeviceNode, DeviceTreeSource, NodeProperty};
