//! Менеджер регионов памяти
//!
//! Отвечает за выделение физических страниц и преобразование в виртуальные адреса.
//! Использует FrameAllocator для выделения смежных страниц.

use crate::frame_allocator::FrameAllocator;
use crate::virtual_address::VirtualAddress;

/// Менеджер регионов памяти.
/// Выделяет физические страницы через FrameAllocator и преобразует в VA.
pub struct RegionManager {
    frame_allocator: &'static dyn FrameAllocator,
    higher_half_base: usize,
}

impl RegionManager {
    /// Создать менеджер регионов
    pub fn new(
        frame_allocator: &'static dyn FrameAllocator,
        higher_half_base: usize,
    ) -> Self {
        Self {
            frame_allocator,
            higher_half_base,
        }
    }

    /// Выделяет до `max_pages` смежных страниц.
    /// Возвращает (виртуальный адрес первой страницы, количество страниц).
    pub fn allocate_pages(&self, max_pages: usize) -> Option<(VirtualAddress, usize)> {
        let (frame, count) = self.frame_allocator.allocate_pages(max_pages)?;
        let phys = frame.page_address().as_usize();
        let va = VirtualAddress::new(phys + self.higher_half_base);
        Some((va, count))
    }
}
