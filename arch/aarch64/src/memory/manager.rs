use crate::memory::allocator::HeapAllocator;
use crate::memory::bump_allocator;
use crate::memory::bump_allocator::BumpAllocError;
use crate::memory::global_allocator::GlobalKernelAllocator;
use crate::memory::layout::MemoryLayout;
use crate::memory::memory_mapper::Aarch64MemoryMapper;
use crate::memory::ram_memory::Aarch64RamMemory;
use crate::memory::virtual_mem::{PageTableManager, VmError, create_page_table_manager};
use kernel_core::console::console;
use kernel_core::printf;
use memory::memory_range::MemoryRange;
use memory::physical::PageAlignedAddress;
use memory::physical_manager::{ManagerError, PhysicalMemoryManager};

// Глобальный аллокатор кучи (не содержит статических полей внутри)
#[global_allocator]
#[unsafe(link_section = ".bss.heap")]
static GLOBAL_ALLOCATOR: GlobalKernelAllocator = GlobalKernelAllocator::new();

/// Центральный менеджер памяти, который владеет всеми компонентами системы памяти.
pub struct MemoryManager {
    heap_allocator: HeapAllocator<
        Aarch64RamMemory,
        Aarch64MemoryMapper<
            'static,
            PhysicalMemoryManager<'static, Aarch64RamMemory>,
            Aarch64RamMemory,
        >,
    >,

    /// Ссылка на PageTableManager для enable_virtual_mode()
    page_table_manager: &'static PageTableManager<
        'static,
        PhysicalMemoryManager<'static, Aarch64RamMemory>,
        Aarch64RamMemory,
    >,
}

impl MemoryManager {
    /// Создать новый менеджер памяти из layout
    ///
    /// Использует embedded_heap для размещения компонентов с self-referential зависимостями.
    pub fn new(memory_layout: MemoryLayout) -> Result<Self, MemorySetupError> {
        let frame_size = memory_layout.heap.frame_size;

        unsafe {
            let heap = bump_allocator::bump_allocator();

            let backend_ptr =
                heap.alloc_ptr(Aarch64RamMemory::new(frame_size))
                    .map_err(|error| MemorySetupError::AllocationError {
                        name: "RAM memory",
                        error,
                    })?;

            let range_ptr = heap
                .alloc_ptr(MemoryRange::new(
                    memory_layout.heap.start,
                    memory_layout.heap.end,
                    memory_layout.heap.frame_size,
                ))
                .map_err(|error| MemorySetupError::AllocationError {
                    name: "memory range",
                    error,
                })?;

            let excluded_regions = [
                (&memory_layout.kernel).clone(),
                (&memory_layout.dtb).clone(),
            ];

            let excluded_memory_range =
                excluded_regions.map(|e| Into::<MemoryRange<PageAlignedAddress>>::into(e));

            let frame_allocator = PhysicalMemoryManager::new(&*backend_ptr, &*range_ptr, &excluded_memory_range)
                .map_err(|error| match error {
                    ManagerError::BitmapCreationFailed(_) => {
                        MemorySetupError::BitmapAllocationError
                    }
                    ManagerError::InvalidRange => MemorySetupError::PhysicalMemoryInvalidRangeError,
                })?;

            let frame_allocator_ptr =
                heap.alloc_ptr(frame_allocator)
                    .map_err(|error| MemorySetupError::AllocationError {
                        name: "physical memory manager",
                        error,
                    })?;

            let ptm = create_page_table_manager(
                &*frame_allocator_ptr,
                &*backend_ptr,
                memory_layout,
                &excluded_regions,
            )
            .map_err(|error| MemorySetupError::VirtualManagerSetupError(error))?;
            let page_table_manager_ptr =
                heap.alloc_ptr(ptm)
                    .map_err(|error| MemorySetupError::AllocationError {
                        name: "page table manager",
                        error,
                    })?;

            let mm = Aarch64MemoryMapper::new(&*frame_allocator_ptr, &*page_table_manager_ptr);
            let memory_mapper_ptr =
                heap.alloc_ptr(mm)
                    .map_err(|error| MemorySetupError::AllocationError {
                        name: "memory mapper",
                        error,
                    })?;

            let heap_allocator = HeapAllocator::new(&*memory_mapper_ptr, &*backend_ptr, frame_size);

            Ok(MemoryManager {
                heap_allocator,
                page_table_manager: &*page_table_manager_ptr,
            })
        }
    }

    /// Инициализировать и активировать систему памяти
    pub unsafe fn enable(&mut self) -> Result<(), MemorySetupError> {
        unsafe {
            // Инициализируем аллокатор кучи
            self.heap_allocator
                .init()
                .or(Err(MemorySetupError::HeapAllocatorInitializationError))?;

            printf!(console(), "Heap allocator initialized!");

            // Устанавливаем глобальный аллокатор
            GLOBAL_ALLOCATOR.set_heap_allocator(&mut self.heap_allocator);

            printf!(console(), "Global allocator set!");

            // Включаем виртуальную память
            self.page_table_manager.enable_virtual_mode();

            Ok(())
        }
    }
}

#[derive(Debug)]
pub enum MemorySetupError {
    AllocationError {
        name: &'static str,
        error: BumpAllocError,
    },
    BitmapAllocationError,
    PhysicalMemoryInvalidRangeError,
    HeapAllocatorInitializationError,
    VirtualManagerSetupError(#[allow(dead_code)] VmError),
}
