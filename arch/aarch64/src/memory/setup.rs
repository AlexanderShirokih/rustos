use crate::memory::allocator::KernelAllocator;
use crate::memory::layout::MemoryLayout;
use crate::memory::memory_mapper::Aarch64MemoryMapper;
use crate::memory::ram_memory::Aarch64RamMemory;
use crate::memory::virtual_mem;
use crate::memory::virtual_mem::PageTableManager;
use memory::memory_range::MemoryRange;
use memory::physical::PhysicalMemoryManager;
use spin::Once;

static FRAME_ALLOCATOR: Once<PhysicalMemoryManager> = Once::new();
static MEMORY_BACKEND: Once<Aarch64RamMemory> = Once::new();
static PAGE_MANAGER: Once<PageTableManager<Aarch64RamMemory>> = Once::new();

static ALLOCATOR: KernelAllocator<Aarch64RamMemory, Aarch64MemoryMapper<Aarch64RamMemory>> =
    KernelAllocator::new();

#[derive(Debug)]
pub enum MemorySetupError {
    AllocatorInitializationError,
    VirtualManagerSetupError,
}

/// Architecture-specific kernel setup for AArch64
pub fn setup_memory() -> Result<(), MemorySetupError> {
    // Get memory regions layout
    let memory_layout = unsafe { MemoryLayout::get() };

    // Create a physical memory manager
    let ram_range = MemoryRange::new(
        memory_layout.heap.start,
        memory_layout.heap.end,
        memory_layout.heap.frame_size,
    );

    let frame_allocator = FRAME_ALLOCATOR.call_once(|| PhysicalMemoryManager::new(&ram_range));

    // Create a physical memory access wrapper
    let memory_backend =
        MEMORY_BACKEND.call_once(|| Aarch64RamMemory::new(memory_layout.heap.frame_size));

    // Create the virtual page table manager
    let page_table_manager = PAGE_MANAGER.call_once(|| {
        virtual_mem::init(frame_allocator, memory_backend, memory_layout)
            .map_err(|_| MemorySetupError::VirtualManagerSetupError)
            .unwrap()
    });

    // Create the physical to virtual memory mapper
    let mapper = Aarch64MemoryMapper::new(frame_allocator, page_table_manager);

    // Initialize the allocator
    ALLOCATOR
        .init(&memory_backend, mapper, &page_table_manager)
        .map_err(|_| MemorySetupError::AllocatorInitializationError)?;

    Ok(())
}
