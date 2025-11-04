use crate::memory::allocator::MemoryMapper;
use crate::memory::virtual_address::VirtualAddress;
use crate::memory::virtual_mem::{Page, PageTableManager, VmError};
use memory::memory_backend::MemoryBackend;
use memory::physical_manager::FrameAllocator;

/// Маппер памяти для AArch64
pub struct Aarch64MemoryMapper<'a, FA: FrameAllocator, B: MemoryBackend> {
    frame_allocator: &'a FA,
    page_table_manager: &'a PageTableManager<'a, FA, B>,
    frame_size: usize,
}

unsafe impl<FA: FrameAllocator, B: MemoryBackend> Send for Aarch64MemoryMapper<'_, FA, B> {}
unsafe impl<FA: FrameAllocator, B: MemoryBackend> Sync for Aarch64MemoryMapper<'_, FA, B> {}

impl<'a, FA: FrameAllocator, B: MemoryBackend> Aarch64MemoryMapper<'a, FA, B> {
    pub fn new(frame_allocator: &'a FA, page_table_manager: &'a PageTableManager<FA, B>) -> Self {
        Aarch64MemoryMapper {
            frame_allocator,
            page_table_manager,
            frame_size: page_table_manager.frame_size(),
        }
    }

    /// Получить ссылку на frame_allocator (безопасно, так как гарантируется владельцем)
    fn frame_allocator(&self) -> &'a FA {
        self.frame_allocator
    }
}

impl<'a, FA: FrameAllocator, B: MemoryBackend> MemoryMapper for Aarch64MemoryMapper<'a, FA, B> {
    fn map_heap_frames(&self, start_addr: usize, size: usize) -> Result<(), MemoryMappingError> {
        let frame_size = self.frame_size;

        // Проверяем входные параметры
        if size == 0 {
            return Ok(());
        }

        // Убеждаемся, что начальный адрес выровнен по странице
        if start_addr % frame_size != 0 {
            return Err(MemoryMappingError::InvalidAddress(
                "Start address must be page-aligned".into(),
            ));
        }

        // Округляем размер вверх до ближайшего кратного frame_size
        let aligned_size = (size + frame_size - 1) / frame_size * frame_size;

        // Получаем менеджер таблиц страниц и аллокатор фреймов
        let page_table_manager = self.page_table_manager;
        let frame_allocator = self.frame_allocator();

        // Отображаем физические фреймы в виртуальную область кучи
        for offset in (0..aligned_size).step_by(frame_size) {
            let virt_addr = VirtualAddress::new(start_addr + offset);
            let page = Page::containing_address(virt_addr, frame_size);

            // Выделяем физический фрейм
            match frame_allocator.allocate_frame() {
                Some(frame) => {
                    // Отображаем его на виртуальный адрес
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
                    // Очистка: размапить все ранее отображенные страницы
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

        // Проверяем входные параметры
        if size == 0 {
            return Ok(());
        }

        // Убеждаемся, что начальный адрес выровнен по странице
        if start_addr % frame_size != 0 {
            return Err(MemoryMappingError::InvalidAddress(
                "Start address must be page-aligned".into(),
            ));
        }

        // Округляем размер вверх до ближайшего кратного frame_size
        let aligned_size = (size + frame_size - 1) / frame_size * frame_size;

        let page_table_manager = self.page_table_manager;

        // Размапить все страницы в диапазоне
        for offset in (0..aligned_size).step_by(frame_size) {
            let virt_addr = VirtualAddress::new(start_addr + offset);
            let page = Page::containing_address(virt_addr, frame_size);

            // Размапить страницу и освободить фрейм
            if let Err(_) = page_table_manager.unmap(page) {
                // Логируем ошибку, но продолжаем размапить другие страницы
                // В реальном ядре лучше использовать правильный механизм логирования
            }
        }

        Ok(())
    }
}

/// Типы ошибок при сбое маппинга памяти
#[derive(Debug, Clone)]
pub enum MemoryMappingError {
    InvalidAddress(&'static str),
    VirtualMappingError(VmError),
    OutOfMemory(&'static str),
    AlreadyMapped,
}
