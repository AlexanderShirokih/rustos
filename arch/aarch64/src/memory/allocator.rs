//! Kernel memory allocator
//!
//! This module provides a heap allocator for the kernel using a first-fit
//! free list algorithm with automatic heap expansion.

use crate::memory::memory_mapper::MemoryMappingError;
use crate::memory::virtual_mem::PageTableManager;
use core::alloc::{GlobalAlloc, Layout};
use core::mem::size_of;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;
use memory::memory_backend::{MemoryBackend, MemoryBackendExt};
use memory::physical::PhysicalAddress;

/// Heap-start virtual address
pub const HEAP_START: usize = 0xFFFF_0000_0000_0000;

/// Initial heap size (1MB)
pub const HEAP_INITIAL_SIZE: usize = 1024 * 1024;

/// Maximum heap size (64MB)
pub const HEAP_MAX_SIZE: usize = 64 * 1024 * 1024;

/// Minimum allocation size (16 bytes)
const MIN_ALLOC_SIZE: usize = 16;

/// Alignment for all allocations
const ALLOC_ALIGN: usize = 8;

/// Error types for allocation failures
#[derive(Debug, Clone, Copy)]
pub enum AllocationError {
    OutOfMemory,
    MappingFailed(MemoryMappingError),
    InvalidLayout,
}

/// Memory mapper trait for virtual memory operations
pub trait MemoryMapper: Send + Sync {
    /// Map physical frames to virtual memory for the heap
    fn map_heap_frames(&self, start_addr: usize, size: usize) -> Result<(), MemoryMappingError>;
    fn unmap_heap_frames(&self, start_addr: usize, size: usize) -> Result<(), MemoryMappingError>;
}

/// A free block in the heap
#[derive(Clone, Copy, Debug)]
struct FreeBlock {
    size: usize,
    next: Option<NonNull<FreeBlock>>,
}

// Safety: FreeBlock is only used within our controlled heap allocator
// with proper synchronization through the Mutex wrapper
unsafe impl Send for FreeBlock {}
unsafe impl Sync for FreeBlock {}

impl FreeBlock {
    /// Create a new free block
    const fn new(size: usize) -> Self {
        FreeBlock { size, next: None }
    }

    /// Split this block if it's large enough for the requested size
    /// Returns the new block created from the split, if any
    fn split(&mut self, requested_size: usize) -> Option<NonNull<FreeBlock>> {
        let aligned_size = align_up(requested_size, ALLOC_ALIGN);
        let block_header_size = size_of::<FreeBlock>();

        // Check if we can split: need space for requested size + new block header + minimum size
        let min_remaining = block_header_size + MIN_ALLOC_SIZE;
        if self.size < aligned_size + min_remaining {
            return None;
        }

        // Calculate the position and size of the new block
        let new_block_offset = aligned_size;
        let new_block_size = self.size - aligned_size;

        // Create the new block at the calculated position
        let new_block_ptr = unsafe {
            let base_ptr = self as *mut FreeBlock as usize;
            let new_ptr = (base_ptr + new_block_offset) as *mut FreeBlock;

            // Initialize the new block
            *new_ptr = FreeBlock::new(new_block_size);

            NonNull::new_unchecked(new_ptr)
        };

        // Update this block's size to the allocated portion
        self.size = aligned_size;

        Some(new_block_ptr)
    }
}

/// A first-fit heap allocator with automatic expansion
pub struct HeapAllocator<B: MemoryBackend + 'static, M: MemoryMapper + 'static> {
    /// Head of the free list
    free_list: Option<NonNull<FreeBlock>>,

    /// Current heap size in bytes
    current_size: usize,

    /// Next allocation position for heap expansion
    next_alloc_addr: AtomicUsize,

    /// Memory mapper for virtual memory operations
    memory_mapper: M,

    /// Backend for reading/writing memory
    memory_backend: &'static B,

    /// Frame size for memory mapping
    frame_size: usize,
}

// Safety: HeapAllocator is designed to be used in a single-threaded context
// or protected by external synchronization (Mutex in KernelAllocator)
unsafe impl<B: MemoryBackend + 'static, M: MemoryMapper + 'static> Send for HeapAllocator<B, M> {}
unsafe impl<B: MemoryBackend + 'static, M: MemoryMapper + 'static> Sync for HeapAllocator<B, M> {}

impl<B: MemoryBackend + 'static, M: MemoryMapper + 'static> HeapAllocator<B, M> {
    /// Create a new heap allocator
    pub fn new(
        memory_mapper: M,
        memory_backend: &'static B,
        frame_size: usize,
    ) -> Self {
        HeapAllocator {
            free_list: None,
            current_size: 0,
            next_alloc_addr: AtomicUsize::new(HEAP_START),
            memory_mapper,
            memory_backend,
            frame_size,
        }
    }

    /// Initialize the allocator with the initial heap space
    pub fn init(&mut self) -> Result<(), AllocationError> {
        self.expand_heap(HEAP_INITIAL_SIZE)
    }

    /// Expand the heap by allocating more memory
    fn expand_heap(&mut self, additional_size: usize) -> Result<(), AllocationError> {
        let aligned_size = align_up(additional_size, self.frame_size);

        // Check if expansion would exceed maximum heap size
        if self.current_size + aligned_size > HEAP_MAX_SIZE {
            return Err(AllocationError::OutOfMemory);
        }

        let mut start_addr = self.next_alloc_addr.load(Ordering::Relaxed);

        loop {
            // Map physical frames to the virtual heap area
            let error = self
                .memory_mapper
                .map_heap_frames(start_addr, aligned_size)
                .err();

            match error {
                Some(e) => match e {
                    MemoryMappingError::AlreadyMapped => {
                        start_addr = start_addr + self.frame_size;
                    }
                    _ => return Err(AllocationError::MappingFailed(e)),
                },

                None => break,
            }
        }

        // Create a new free block for the expanded area
        let block_addr = PhysicalAddress(start_addr);
        let block_size = aligned_size - size_of::<FreeBlock>();
        let new_block = FreeBlock::new(block_size);

        // Write the new block to memory
        self.memory_backend.write(block_addr, new_block);

        // Add to free list
        self.add_to_free_list(start_addr);

        // Update allocator state
        self.current_size += aligned_size;
        self.next_alloc_addr
            .store(start_addr + aligned_size, Ordering::Relaxed);

        Ok(())
    }

    /// Add a block to the free list
    fn add_to_free_list(&mut self, addr: usize) {
        let block_ptr = NonNull::new(addr as *mut FreeBlock).expect("Invalid address");

        // Read the block, update its next pointer, and write it back
        let mut block: FreeBlock = self.memory_backend.read(PhysicalAddress(addr));
        block.next = self.free_list;
        self.memory_backend.write(PhysicalAddress(addr), block);

        // Update the free list head
        self.free_list = Some(block_ptr);
    }

    /// Find and remove a suitable block from the free list
    fn find_free_block(&mut self, size: usize) -> Option<NonNull<FreeBlock>> {
        let mut current = self.free_list;
        let mut prev: Option<NonNull<FreeBlock>> = None;

        while let Some(block_ptr) = current {
            let addr = block_ptr.as_ptr() as usize;
            let mut block: FreeBlock = self.memory_backend.read(PhysicalAddress(addr));

            // Check if this block is large enough
            if block.size >= size {
                // Remove from free list
                if let Some(prev_ptr) = prev {
                    let mut prev_block: FreeBlock = self
                        .memory_backend
                        .read(PhysicalAddress(prev_ptr.as_ptr() as usize));
                    prev_block.next = block.next;
                    self.memory_backend
                        .write(PhysicalAddress(prev_ptr.as_ptr() as usize), prev_block);
                } else {
                    self.free_list = block.next;
                }

                // Split the block if it's significantly larger than needed
                if let Some(new_block_ptr) = block.split(size) {
                    let new_addr = new_block_ptr.as_ptr() as usize;
                    self.add_to_free_list(new_addr);
                }

                // Write back the allocated block
                self.memory_backend.write(PhysicalAddress(addr), block);
                return Some(block_ptr);
            }

            // Move to next block
            prev = current;
            current = block.next;
        }

        None
    }

    /// Allocate memory with the given layout
    pub fn allocate(&mut self, layout: Layout) -> Result<NonNull<u8>, AllocationError> {
        if layout.size() == 0 {
            return Err(AllocationError::InvalidLayout);
        }

        let size = layout.size().max(MIN_ALLOC_SIZE);

        // Try to find a suitable block
        if let Some(block_ptr) = self.find_free_block(size) {
            let ptr = block_ptr.as_ptr() as *mut u8;
            return Ok(NonNull::new(ptr).expect("Block pointer should not be null"));
        }

        // No suitable block found, expand the heap
        let needed_size = size + size_of::<FreeBlock>();
        let expand_size = needed_size.max(self.frame_size);

        self.expand_heap(expand_size)?;

        // Try allocation again after expansion
        if let Some(block_ptr) = self.find_free_block(size) {
            let ptr = block_ptr.as_ptr() as *mut u8;
            Ok(NonNull::new(ptr).expect("Block pointer should not be null"))
        } else {
            Err(AllocationError::OutOfMemory)
        }
    }

    /// Deallocate memory
    pub fn deallocate(&mut self, ptr: NonNull<u8>) {
        let addr = ptr.as_ptr() as usize;
        self.add_to_free_list(addr);

        // TODO: Implement coalescing of adjacent free blocks to reduce fragmentation
    }

    /// Get current heap statistics
    pub fn stats(&self) -> HeapStats {
        HeapStats {
            total_size: self.current_size,
            max_size: HEAP_MAX_SIZE,
        }
    }
}

/// Heap statistics for monitoring
#[derive(Debug, Clone, Copy)]
pub struct HeapStats {
    pub total_size: usize,
    pub max_size: usize,
}

/// Global kernel allocator
pub struct KernelAllocator<B: MemoryBackend + 'static, M: MemoryMapper + 'static> {
    inner: Mutex<Option<HeapAllocator<B, M>>>,
}

impl<B: MemoryBackend + 'static, M: MemoryMapper + 'static> KernelAllocator<B, M> {
    /// Create a new kernel allocator
    pub const fn new() -> Self {
        KernelAllocator {
            inner: Mutex::new(None),
        }
    }

    /// Initialize the allocator
    pub fn init(
        &self,
        backend: &'static B,
        memory_mapper: M,
        page_table_manager: &PageTableManager<B>,
    ) -> Result<(), AllocationError> {
        let mut inner = self.inner.lock();
        if inner.is_none() {
            let mut allocator =
                HeapAllocator::new(memory_mapper, backend, page_table_manager.frame_size());
            allocator.init()?;
            *inner = Some(allocator);
        }
        Ok(())
    }

    /// Get heap statistics
    pub fn stats(&self) -> Option<HeapStats> {
        let inner = self.inner.lock();
        inner.as_ref().map(|allocator| allocator.stats())
    }
}

unsafe impl<B: MemoryBackend + 'static, M: MemoryMapper + 'static> GlobalAlloc for KernelAllocator<B, M> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut inner = self.inner.lock();

        if let Some(ref mut allocator) = *inner {
            match allocator.allocate(layout) {
                Ok(ptr) => ptr.as_ptr(),
                Err(_) => core::ptr::null_mut(),
            }
        } else {
            // Return null if the allocator is not initialized
            core::ptr::null_mut()
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if ptr.is_null() {
            return;
        }

        let mut inner = self.inner.lock();

        if let Some(ref mut allocator) = *inner {
            if let Some(non_null_ptr) = NonNull::new(ptr) {
                allocator.deallocate(non_null_ptr);
            }
        }
        // If allocator is not initialized, we silently ignore the deallocation
        // This is consistent with the behavior of many allocators
    }
}

/// Helper function to align up to the specified alignment
const fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}
