//! Memory management subsystem
//!
//! This module provides memory management functionality for the kernel

use crate::kernel::Kernel;
use crate::kernel::kernel::AbstractKernel;

pub mod allocator;
pub mod memory_map;
pub(crate) mod physical;

/// Initialize memory subsystem
pub fn init(kernel: &Kernel) {
    // Initialize physical memory manager
    let mut pmm = physical::init(kernel);

    AbstractKernel::setup_memory(kernel, &mut pmm);
}
