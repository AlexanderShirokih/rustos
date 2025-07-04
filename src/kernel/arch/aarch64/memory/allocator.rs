//! Architecture-specific memory allocator implementation for AArch64
//!
//! This module provides the AArch64-specific implementation of the memory mapper
//! for the kernel heap allocator.

use crate::kernel::arch::aarch64::memory::virtual_mem::{
    EntryFlags, Page, PageTableManager, VirtualAddress,
};
use crate::kernel::memory::allocator::MemoryMapper;
use crate::kernel::memory::physical::{FrameAllocator, PhysicalMemoryManager};

/// AArch64-specific memory mapper
pub struct Aarch64MemoryMapper {
    page_table_manager: *mut PageTableManager,
    frame_allocator: *mut PhysicalMemoryManager,
    frame_size: usize,
}

// Safety: The pointers are only used in a controlled environment within our kernel
// and we ensure proper synchronization through atomic operations.
unsafe impl Send for Aarch64MemoryMapper {}
unsafe impl Sync for Aarch64MemoryMapper {}

impl Aarch64MemoryMapper {
    /// Create a new AArch64 memory mapper
    pub fn new(
        page_table_manager: &mut PageTableManager,
        frame_allocator: &mut PhysicalMemoryManager,
    ) -> Self {
        Aarch64MemoryMapper {
            page_table_manager: page_table_manager as *mut PageTableManager,
            frame_allocator: frame_allocator as *mut PhysicalMemoryManager,
            frame_size: frame_allocator.frame_size(),
        }
    }
}

impl MemoryMapper for Aarch64MemoryMapper {
    fn map_heap_frames(&self, start_addr: usize, size: usize) -> Result<(), &'static str> {
        let frame_size = self.frame_size;

        // Get the page table manager and frame allocator
        let page_table_manager = unsafe { &mut *self.page_table_manager };
        let frame_allocator = unsafe { &mut *self.frame_allocator };

        // Map physical frames to the virtual heap area
        for offset in (0..size).step_by(frame_size) {
            let virt_addr = VirtualAddress::new(start_addr + offset);
            let page = Page::containing_address(virt_addr, frame_size);

            // Allocate a physical frame
            if let Some(frame) = frame_allocator.allocate_frame() {
                // Map it to the virtual address
                if let Err(e) =
                    page_table_manager.map(page, frame, EntryFlags::KERNEL_RW, frame_allocator)
                {
                    return Err(e);
                }
            } else {
                // Out of memory
                return Err("Out of memory: failed to allocate frame for heap expansion");
            }
        }

        Ok(())
    }
}

/// Initialize the memory allocator
pub fn init(
    page_table_manager: &mut PageTableManager,
    frame_allocator: &mut PhysicalMemoryManager,
) {
    // Create the memory mapper
    let memory_mapper = Aarch64MemoryMapper::new(page_table_manager, frame_allocator);

    // Initialize the allocator
    crate::kernel::memory::allocator::init_allocator(memory_mapper, frame_allocator.frame_size());
}
