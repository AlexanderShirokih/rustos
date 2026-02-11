//! Драйверы устройств AArch64.

#![no_std]

extern crate alloc;

mod commons;
pub mod fdt_adapter;
pub mod tree_ext;
pub use commons::*;
pub use fdt_adapter::adapt_tree;
