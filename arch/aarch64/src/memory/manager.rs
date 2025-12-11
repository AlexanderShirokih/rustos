use crate::memory::allocator::HeapAllocator;
use crate::memory::global_allocator::KernelHeapAllocator;
use crate::memory::memory_mapper::Aarch64MemoryMapper;
use crate::memory::mmu::{Mmu, RootTableConfig};
use crate::memory::ram_memory::Aarch64RamMemory;
use crate::memory::virtual_mem::{PageTableManager, create_page_table_manager};
use aarch64_paging::MemoryLayout;
use alloc::sync::Arc;
use kernel::console::stdout;
use kernel::debug;
use memory::memory_range::MemoryRange;
use memory::physical::PageAlignedAddress;
use memory::physical_manager::PhysicalMemoryManager;

type FrameAllocatorType = PhysicalMemoryManager;
type PageTableManagerType = PageTableManager<FrameAllocatorType, Aarch64RamMemory>;

/// Центральный менеджер памяти, который владеет всеми компонентами системы памяти.
pub struct MemoryManager {
    heap_allocator: KernelHeapAllocator,
    page_table_manager: Arc<PageTableManagerType>,
}

impl MemoryManager {
    pub fn new(memory_layout: MemoryLayout) -> Result<Self, MemorySetupError> {
        let heap = memory_layout
            .heap()
            .next()
            .ok_or(MemorySetupError::IllegalStateError)?;

        let frame_size = heap.frame_size();

        let backend = Aarch64RamMemory::new(frame_size);

        let heap_range: MemoryRange<PageAlignedAddress> = heap.clone().into();

        let identity_map_regions = memory_layout
            .iter()
            .filter(|region| region.identity_map)
            .map(|region| MemoryRange::new(region.start, region.end, region.frame_size()));

        let frame_allocator = Arc::new(PhysicalMemoryManager::new(
            &heap_range,
            identity_map_regions,
        ));

        let page_table_manager = Arc::new(
            create_page_table_manager(frame_allocator.clone(), backend, memory_layout)
                .map_err(|_| MemorySetupError::VirtualManagerSetupError)?,
        );

        // Создаём MemoryMapper с Arc ссылками
        let memory_mapper = Arc::new(Aarch64MemoryMapper::new(
            frame_allocator.clone(),
            page_table_manager.clone(),
        ));

        let heap_allocator = HeapAllocator::new(memory_mapper, backend, frame_size);

        Ok(MemoryManager {
            heap_allocator,
            page_table_manager,
        })
    }

    pub fn enable(mut self) -> Result<KernelHeapAllocator, MemorySetupError> {
        // Включаем виртуальную память (таблицы страниц уже созданы и заполнены)
        let root_page = self
            .page_table_manager
            .root_frame()
            .page_address()
            .as_physical_address();

        let mmu = Mmu::new();
        mmu.enable(RootTableConfig::new(root_page));

        debug!(stdout(), "Paging enabled!");

        // Инициализируем аллокатор кучи
        self.heap_allocator
            .init()
            .or(Err(MemorySetupError::HeapAllocatorInitializationError))?;

        Ok(self.heap_allocator)
    }
}

#[derive(Debug)]
pub enum MemorySetupError {
    HeapAllocatorInitializationError,
    VirtualManagerSetupError,
    IllegalStateError,
}
