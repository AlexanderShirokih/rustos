//! Architecture-specific memory allocator implementation for AArch64
//!
//! This module provides the AArch64-specific implementation of the memory mapper
//! for the kernel heap allocator.

use crate::memory::allocator::MemoryMapper;
use crate::memory::virtual_address::VirtualAddress;
use crate::memory::virtual_mem::{Page, PageTableManager, VmError};
use memory::memory_backend::MemoryBackend;
use memory::physical::FrameAllocator;

/// AArch64-specific memory mapper
pub struct Aarch64MemoryMapper<B: MemoryBackend + 'static> {
    frame_allocator: &'static dyn FrameAllocator,
    page_table_manager: &'static PageTableManager<B>,
    frame_size: usize,
}

// Safety: The pointers are only used in a controlled environment within our kernel,
// and we ensure proper synchronization through atomic operations.
unsafe impl<B: MemoryBackend + 'static> Send for Aarch64MemoryMapper<B> {}
unsafe impl<B: MemoryBackend + 'static> Sync for Aarch64MemoryMapper<B> {}

impl<B: MemoryBackend + 'static> Aarch64MemoryMapper<B> {
    /// Create a new AArch64 memory mapper
    pub fn new(
        frame_allocator: &'static dyn FrameAllocator,
        page_table_manager: &'static PageTableManager<B>,
    ) -> Self {
        Aarch64MemoryMapper {
            frame_allocator,
            page_table_manager,
            frame_size: page_table_manager.frame_size(),
        }
    }
}

impl<B: MemoryBackend + 'static> MemoryMapper for Aarch64MemoryMapper<B> {
    fn map_heap_frames(&self, start_addr: usize, size: usize) -> Result<(), MemoryMappingError> {
        let frame_size = self.frame_size;

        // Validate input parameters
        if size == 0 {
            return Ok(());
        }

        // Ensure the start address is page-aligned
        if start_addr % frame_size != 0 {
            return Err(MemoryMappingError::InvalidAddress(
                "Start address must be page-aligned".into(),
            ));
        }

        // Round size up to the nearest multiple of frame_size
        let aligned_size = (size + frame_size - 1) / frame_size * frame_size;

        // Get the page table manager and frame allocator
        let page_table_manager = self.page_table_manager;
        let frame_allocator = self.frame_allocator;

        // Map physical frames to the virtual heap area
        for offset in (0..aligned_size).step_by(frame_size) {
            let virt_addr = VirtualAddress::new(start_addr + offset);
            let page = Page::containing_address(virt_addr, frame_size);

            // Allocate a physical frame
            match frame_allocator.allocate_frame() {
                Some(frame) => {
                    // Map it to the virtual address
                    match page_table_manager
                        .map(page, frame, page_table_manager.heap.flags)
                        .map_err(|e| match e {
                            VmError::AlreadyMapped => MemoryMappingError::AlreadyMapped,
                            _ => MemoryMappingError::VirtualMappingError(e),
                        }) {
                        Ok(()) => {
                            // mapped_pages.push(page);
                        }

                        Err(e) => return Err(e),
                    }
                }
                None => {
                    // Cleanup: unmap all previously mapped pages
                    // for mapped_page in mapped_pages {
                    //     let _ = page_table_manager.unmap(mapped_page);
                    // }
                    return Err(MemoryMappingError::OutOfMemory(
                        "Failed to allocate frame for heap expansion".into(),
                    ));
                }
            }
        }

        Ok(())
    }

    fn unmap_heap_frames(&self, start_addr: usize, size: usize) -> Result<(), MemoryMappingError> {
        let frame_size = self.frame_size;

        // Validate input parameters
        if size == 0 {
            return Ok(());
        }

        // Ensure the start address is page-aligned
        if start_addr % frame_size != 0 {
            return Err(MemoryMappingError::InvalidAddress(
                "Start address must be page-aligned".into(),
            ));
        }

        // Round size up to the nearest multiple of frame_size
        let aligned_size = (size + frame_size - 1) / frame_size * frame_size;

        let page_table_manager = self.page_table_manager;

        // Unmap all pages in the range
        for offset in (0..aligned_size).step_by(frame_size) {
            let virt_addr = VirtualAddress::new(start_addr + offset);
            let page = Page::containing_address(virt_addr, frame_size);

            // Unmap the page and free the frame
            if let Err(_) = page_table_manager.unmap(page) {
                // Log the error but continue unmapping other pages
                // In a real kernel, you might want to use a proper logging mechanism
            }
        }

        Ok(())
    }
}

/// Error types for allocation failures
#[derive(Debug, Clone, Copy)]
pub enum MemoryMappingError {
    InvalidAddress(&'static str),
    VirtualMappingError(VmError),
    OutOfMemory(&'static str),
    AlreadyMapped,
}
