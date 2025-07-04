//! Memory layout definitions
//!
//! This module defines the memory layout for the system, including
//! physical memory regions, device memory regions, and other memory-related constants.

use crate::kernel::memory::physical::PhysicalAddress;

// Memory layout constants from a linker script
unsafe extern "C" {
    static _stack_bottom: u8;
    static _stack_top: u8;
    static _kernel_start: u8;
    static _kernel_end: u8;
}

// Physical memory layout
pub struct MemoryLayout {
    pub stack_start: PhysicalAddress,
    pub stack_end: PhysicalAddress,
    pub kernel_start: PhysicalAddress,
    pub kernel_end: PhysicalAddress,
    pub memory_start: PhysicalAddress,
    pub memory_end: PhysicalAddress,
}

impl MemoryLayout {
    pub fn get() -> Self {
        // Get addresses from linker symbols if possible
        let stack_start = unsafe { PhysicalAddress::new(&_stack_bottom as *const _ as usize) };
        let stack_end = unsafe { PhysicalAddress::new(&_stack_top as *const _ as usize) };
        let kernel_start = unsafe { PhysicalAddress::new(&_kernel_start as *const _ as usize) };
        let kernel_end = unsafe { PhysicalAddress::new(&_kernel_end as *const _ as usize) };

        // Available memory starts after the kernel and ends at 128MB
        let memory_start = kernel_end;
        let memory_end = kernel_end.offset_bytes(0x0800_0000); // 128MB total

        MemoryLayout {
            stack_start,
            stack_end,
            kernel_start,
            kernel_end,
            memory_start,
            memory_end,
        }
    }
}
