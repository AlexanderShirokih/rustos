use crate::kernel::arch::aarch64::memory::allocator;
use crate::kernel::arch::aarch64::memory::memory::MemoryLayout;
use crate::kernel::arch::aarch64::memory::virtual_mem;
use crate::kernel::kernel::AbstractKernel;
use crate::kernel::memory::physical::PhysicalMemoryManager;

/// Architecture-specific kernel trait for AArch64
pub trait Aarch64Kernel {
    fn setup_memory(&self, pmm: &mut PhysicalMemoryManager) -> ();
}

impl<T: AbstractKernel> Aarch64Kernel for T {
    fn setup_memory(&self, pmm: &mut PhysicalMemoryManager) -> () {
        // Get the memory layout
        let memory_layout = MemoryLayout::get();

        // Initialize virtual memory with the memory layout
        let mut page_table_manager = virtual_mem::init(pmm, &memory_layout);

        page_table_manager.activate();

        // Initialize heap allocator
        allocator::init(&mut page_table_manager, pmm);
    }
}
