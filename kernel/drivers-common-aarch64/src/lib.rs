//! Драйверы устройств AArch64.

#![no_std]

extern crate alloc;

mod commons;
pub mod device_windows;
pub mod fdt_adapter;
pub mod gic_interrupt;
pub mod tree_ext;
pub use commons::*;
pub use fdt_adapter::adapt_to_fdt_tree;
