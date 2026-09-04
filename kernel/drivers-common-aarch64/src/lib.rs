//! Драйверы устройств AArch64.

#![no_std]
#![allow(clippy::cast_possible_truncation)]

extern crate alloc;

pub mod affinity_frames;
mod commons;
pub mod device_windows;
pub mod fdt_adapter;
pub mod gic_interrupt;
pub mod ipi_affinity;
pub mod tree_ext;
pub use commons::*;
pub use fdt_adapter::adapt_to_fdt_tree;
