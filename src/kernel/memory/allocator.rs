//! Kernel memory allocator
//!
//! This module provides a common heap allocator for the kernel.

use core::alloc::{GlobalAlloc, Layout};
use core::mem::size_of;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

// Forward declaration of Aarch64MemoryMapper
use crate::kernel::arch::aarch64::memory::allocator::Aarch64MemoryMapper;

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

/// Memory mapper trait for virtual memory operations
pub trait MemoryMapper: Send + Sync {
    /// Map physical frames to virtual memory for the heap
    fn map_heap_frames(&self, start_addr: usize, size: usize) -> Result<(), &'static str>;
}

/// A free block in the heap
struct FreeBlock {
    size: usize,
    next: Option<NonNull<FreeBlock>>,
}

// Safety: FreeBlock is only used in a controlled environment within our kernel
// and we ensure proper synchronization through the Mutex in KernelAllocator
unsafe impl Send for FreeBlock {}
unsafe impl Sync for FreeBlock {}

impl FreeBlock {
    /// Create a new free block
    pub fn new(size: usize) -> Self {
        FreeBlock { size, next: None }
    }

    /// Split this block if it's too large for the requested size
    pub fn split(&mut self, size: usize) -> Option<NonNull<FreeBlock>> {
        let aligned_size = align_up(size, ALLOC_ALIGN);

        // Only split if we can create a new block of at least MIN_ALLOC_SIZE
        if self.size >= aligned_size + MIN_ALLOC_SIZE + size_of::<FreeBlock>() {
            let new_block_ptr = (self as *mut FreeBlock as usize + aligned_size) as *mut FreeBlock;
            let new_block_size = self.size - aligned_size - size_of::<FreeBlock>();

            // Create the new block
            unsafe {
                new_block_ptr.write(FreeBlock::new(new_block_size));
                let new_block = NonNull::new_unchecked(new_block_ptr);

                // Update this block's size
                self.size = aligned_size;

                Some(new_block)
            }
        } else {
            None
        }
    }
}

/// A simple first-fit allocator
pub struct BumpAllocator<M: MemoryMapper> {
    /// Free list head
    free_list: Option<NonNull<FreeBlock>>,

    /// Current heap size
    current_size: usize,

    /// Next allocation position (for when we need to expand)
    next_alloc: AtomicUsize,

    /// Memory mapper for virtual memory operations
    memory_mapper: M,

    /// Frame size
    frame_size: usize,
}

// Safety: BumpAllocator is only used in a controlled environment within our kernel
// and we ensure proper synchronization through the Mutex in KernelAllocator.
// The NonNull<FreeBlock> pointers are only used within the allocator and never shared.
unsafe impl<M: MemoryMapper> Send for BumpAllocator<M> {}
unsafe impl<M: MemoryMapper> Sync for BumpAllocator<M> {}

impl<M: MemoryMapper> BumpAllocator<M> {
    /// Create a new empty allocator
    pub fn new(memory_mapper: M, frame_size: usize) -> Self {
        BumpAllocator {
            free_list: None,
            current_size: 0,
            next_alloc: AtomicUsize::new(HEAP_START),
            memory_mapper,
            frame_size,
        }
    }

    /// Initialize the allocator with the initial heap space
    pub fn init(&mut self) {
        // Allocate initial heap space
        self.expand(HEAP_INITIAL_SIZE);
    }

    /// Expand the heap by allocating more memory
    fn expand(&mut self, additional_size: usize) {
        let aligned_size = align_up(additional_size, self.frame_size);
        let start_addr = self.next_alloc.load(Ordering::Relaxed);

        // Map physical frames to the virtual heap area
        if let Err(e) = self.memory_mapper.map_heap_frames(start_addr, aligned_size) {
            panic!("Failed to map memory for heap expansion: {}", e);
        }

        // Create a new free block for the expanded area
        let block_ptr = start_addr as *mut FreeBlock;
        unsafe {
            block_ptr.write(FreeBlock::new(aligned_size - size_of::<FreeBlock>()));
            let block = NonNull::new_unchecked(block_ptr);

            // Add to free list
            self.add_to_free_list(block);
        }

        // Update allocator state
        self.current_size += aligned_size;
        self.next_alloc
            .store(start_addr + aligned_size, Ordering::Relaxed);
    }

    /// Add a block to the free list
    fn add_to_free_list(&mut self, mut block: NonNull<FreeBlock>) {
        unsafe {
            // Insert at the head of the list
            block.as_mut().next = self.free_list;
            self.free_list = Some(block);
        }
    }

    /// Allocate memory
    pub fn allocate(&mut self, layout: Layout) -> *mut u8 {
        let size = layout.size().max(MIN_ALLOC_SIZE);

        // Try to find a suitable block in the free list
        let mut current = self.free_list;
        let mut prev: Option<NonNull<FreeBlock>> = None;

        while let Some(mut block_ptr) = current {
            let block = unsafe { block_ptr.as_ref() };

            // Check if this block is large enough
            if block.size >= size {
                // Remove from free list
                unsafe {
                    if let Some(mut prev_ptr) = prev {
                        prev_ptr.as_mut().next = block.next;
                    } else {
                        self.free_list = block.next;
                    }

                    // Split if necessary
                    if let Some(new_block) = block_ptr.as_mut().split(size) {
                        self.add_to_free_list(new_block);
                    }

                    // Return the allocation
                    return block_ptr.as_ptr() as *mut u8;
                }
            }

            // Move to next block
            prev = current;
            current = block.next;
        }

        // No suitable block found, expand the heap
        let needed_size = size + size_of::<FreeBlock>();
        let expand_size = needed_size.max(self.frame_size);

        // Check if we're within the maximum heap size
        if self.current_size + expand_size <= HEAP_MAX_SIZE {
            self.expand(expand_size);
            self.allocate(layout) // Try again with expanded heap
        } else {
            panic!("Heap allocation failed: maximum heap size reached");
        }
    }

    /// Deallocate memory
    pub fn deallocate(&mut self, ptr: *mut u8, _layout: Layout) {
        if ptr.is_null() {
            return;
        }

        let block_ptr = ptr as *mut FreeBlock;

        // Add the block back to the free list
        if let Some(block) = NonNull::new(block_ptr) {
            self.add_to_free_list(block);
        }

        // TODO: Coalesce adjacent free blocks to reduce fragmentation
    }
}

/// Global kernel allocator
pub struct KernelAllocator {
    inner: Mutex<Option<BumpAllocator<Aarch64MemoryMapper>>>,
}

impl KernelAllocator {
    /// Create a new kernel allocator
    pub const fn new() -> Self {
        KernelAllocator {
            inner: Mutex::new(None),
        }
    }

    /// Initialize the allocator
    pub fn init(&self, memory_mapper: Aarch64MemoryMapper, frame_size: usize) {
        let mut inner = self.inner.lock();
        if inner.is_none() {
            let mut allocator = BumpAllocator::new(memory_mapper, frame_size);
            allocator.init();
            *inner = Some(allocator);
        }
    }
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut inner = self.inner.lock();

        if let Some(ref mut allocator) = *inner {
            allocator.allocate(layout)
        } else {
            panic!("Allocator used before initialization");
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut inner = self.inner.lock();

        if let Some(ref mut allocator) = *inner {
            allocator.deallocate(ptr, layout)
        } else {
            panic!("Allocator used before initialization");
        }
    }
}

#[global_allocator]
pub static ALLOCATOR: KernelAllocator = KernelAllocator::new();

/// Initialize the global allocator
pub fn init_allocator(memory_mapper: Aarch64MemoryMapper, frame_size: usize) {
    ALLOCATOR.init(memory_mapper, frame_size);
}

/// Helper function to align up to the specified alignment
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}
