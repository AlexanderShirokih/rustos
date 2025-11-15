use crate::memory::allocator::MemoryMapper;
use crate::memory::virtual_address::VirtualAddress;
use crate::memory::virtual_mem::{Page, PageTableManager, VmError};
use kernel_core::console::console;
use kernel_core::debug;
use memory::memory_backend::MemoryBackend;
use memory::physical::PageAlignedAddress;
use memory::physical_manager::FrameAllocator;

/// Маппер памяти для AArch64
pub struct Aarch64MemoryMapper<'a, FA: FrameAllocator, B: MemoryBackend> {
    frame_allocator: &'a FA,
    page_table_manager: &'a PageTableManager<'a, FA, B>,
}

impl<'a, FA: FrameAllocator, B: MemoryBackend> Aarch64MemoryMapper<'a, FA, B> {
    pub fn new(frame_allocator: &'a FA, page_table_manager: &'a PageTableManager<FA, B>) -> Self {
        Aarch64MemoryMapper {
            frame_allocator,
            page_table_manager,
        }
    }
}

impl<'a, FA: FrameAllocator, B: MemoryBackend> MemoryMapper for Aarch64MemoryMapper<'a, FA, B> {
    fn map_heap_frames(
        &self,
        start_address: PageAlignedAddress,
        size: usize,
    ) -> Result<(), MemoryMappingError> {
        if size == 0 {
            return Ok(());
        }

        let page_size = PageAlignedAddress::alignment();
        let page_count = (size + page_size - 1) / page_size;

        let page_table_manager = self.page_table_manager;
        let frame_allocator = self.frame_allocator;

        for i in 0..page_count {
            let va = VirtualAddress::new(start_address.as_usize() + i * page_size);
            let page = Page::containing_address(va);

            let frame = frame_allocator
                .allocate_frame()
                .ok_or_else(|| MemoryMappingError::OutOfMemory)?;

            debug!(
                console(),
                "Mapping page {:#x} to frame {:#x}",
                va.as_usize(),
                frame.number()
            );

            page_table_manager
                .map(page, frame, self.page_table_manager.heap_flags())
                .map_err(|e| match e {
                    VmError::AlreadyMapped => MemoryMappingError::AlreadyMapped,
                    _ => MemoryMappingError::VirtualMappingError,
                })?;
        }

        Ok(())
    }

    fn unmap_heap_frames(
        &self,
        start_address: PageAlignedAddress,
        size: usize,
    ) -> Result<(), MemoryMappingError> {
        // Проверяем входные параметры
        if size == 0 {
            return Ok(());
        }

        // Округляем размер вверх до ближайшего кратного alignment
        let alignment = PageAlignedAddress::alignment();
        let aligned_size = (size + alignment - 1) / alignment * alignment;

        let page_table_manager = self.page_table_manager;

        // Размапить все страницы в диапазоне
        for offset in (0..aligned_size).step_by(alignment) {
            let virt_addr = VirtualAddress::new(start_address.as_usize() + offset);
            let page = Page::containing_address(virt_addr);

            // Размапить страницу и освободить фрейм
            if let Err(_) = page_table_manager.unmap(page) {
                // Логируем ошибку, но продолжаем размапить другие страницы
                // TODO: логировать ошибку
            }
        }

        Ok(())
    }
}

/// Типы ошибок при сбое маппинга памяти
#[derive(Debug, Clone)]
pub enum MemoryMappingError {
    VirtualMappingError,
    OutOfMemory,
    AlreadyMapped,
}
