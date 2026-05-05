//! Общие абстракции для драйверов устройств.
//!
//! Содержит трейты, типы и макросы для реализации драйверов.

#![no_std]
extern crate alloc;

pub mod capabilities;
pub mod driver;
pub mod probe;
pub mod scanner;
pub mod services;
pub mod tree;

mod registry;

pub use capabilities::*;
pub use driver::*;
pub use memory::mem_flags::*;
pub use registry::*;
pub use scanner::{DriverInfo, ProbeFn};
pub use tree::{DeviceNode, DeviceTreeSource, NodeProperty};
