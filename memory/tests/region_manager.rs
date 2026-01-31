mod common;

use core::sync::atomic::{AtomicUsize, Ordering};
use memory::frame::Frame;
use memory::frame_allocator::{FrameAllocator, FrameError, ReserveFrameError};
use memory::physical_address::PageAlignedAddress;
use memory::region_manager::RegionManager;

// =============================================================================
// Мок-реализация FrameAllocator для тестов
// =============================================================================

const PAGE_SIZE: usize = 4096;

/// Мок-аллокатор с настраиваемым поведением
struct MockFrameAllocator {
    /// Базовый физический адрес
    base_pa: usize,
    /// Максимальное количество страниц для выделения за раз
    max_pages_per_alloc: usize,
    /// Общее количество доступных страниц
    total_pages: usize,
    /// Счётчик выделенных страниц
    allocated: AtomicUsize,
}

impl MockFrameAllocator {
    fn new(base_pa: usize, total_pages: usize, max_pages_per_alloc: usize) -> Self {
        Self {
            base_pa,
            total_pages,
            max_pages_per_alloc,
            allocated: AtomicUsize::new(0),
        }
    }

    /// Создаёт аллокатор, который всегда возвращает None
    fn empty() -> Self {
        Self::new(0, 0, 0)
    }
}

impl FrameAllocator for MockFrameAllocator {
    fn reserve_frames_exact(
        &self,
        from_inclusive: Frame,
        _to_exclusive: Frame,
    ) -> Result<Frame, ReserveFrameError> {
        Ok(from_inclusive)
    }

    fn allocate_frame(&self) -> Option<Frame> {
        let page = self.allocated.fetch_add(1, Ordering::Relaxed);
        if page >= self.total_pages {
            self.allocated.fetch_sub(1, Ordering::Relaxed);
            return None;
        }
        let addr = self.base_pa + page * PAGE_SIZE;
        Some(Frame::from(
            PageAlignedAddress::from_usize(addr).expect("should be aligned"),
        ))
    }

    fn allocate_pages(&self, max_count: usize) -> Option<(Frame, usize)> {
        if max_count == 0 || self.total_pages == 0 {
            return None;
        }

        let current = self.allocated.load(Ordering::Relaxed);
        if current >= self.total_pages {
            return None;
        }

        let remaining = self.total_pages - current;
        let count = max_count.min(remaining).min(self.max_pages_per_alloc);

        if count == 0 {
            return None;
        }

        let start_page = self.allocated.fetch_add(count, Ordering::Relaxed);
        if start_page >= self.total_pages {
            self.allocated.fetch_sub(count, Ordering::Relaxed);
            return None;
        }

        let addr = self.base_pa + start_page * PAGE_SIZE;
        Some((
            Frame::from(PageAlignedAddress::from_usize(addr).expect("should be aligned")),
            count,
        ))
    }

    fn deallocate_frame(&self, _frame: Frame) -> Result<(), FrameError> {
        Ok(())
    }

    fn is_allocated(&self, _frame: Frame) -> bool {
        false
    }
}

// =============================================================================
// Тесты RegionManager
// =============================================================================

#[test]
fn allocate_pages_returns_correct_virtual_address() {
    let base_pa = 0x1000_0000; // 256 MB
    let higher_half_base = 0xFFFF_0000_0000_0000usize;

    let allocator = Box::leak(Box::new(MockFrameAllocator::new(base_pa, 100, 100)));
    let manager = RegionManager::new(allocator, higher_half_base);

    let result = manager.allocate_pages(10);
    assert!(result.is_some(), "allocation should succeed");

    let (va, count) = result.unwrap();
    assert_eq!(count, 10, "should allocate requested pages");

    // VA = PA + higher_half_base
    let expected_va = base_pa + higher_half_base;
    assert_eq!(
        va.as_usize(),
        expected_va,
        "VA should be PA + higher_half_base"
    );
}

#[test]
fn allocate_pages_with_zero_higher_half_base() {
    let base_pa = 0x8000_0000; // 2 GB
    let higher_half_base = 0; // identity mapping

    let allocator = Box::leak(Box::new(MockFrameAllocator::new(base_pa, 50, 50)));
    let manager = RegionManager::new(allocator, higher_half_base);

    let result = manager.allocate_pages(5);
    assert!(result.is_some());

    let (va, count) = result.unwrap();
    assert_eq!(count, 5);
    // С higher_half_base = 0, VA == PA
    assert_eq!(va.as_usize(), base_pa, "VA should equal PA when higher_half_base is 0");
}

#[test]
fn allocate_pages_returns_none_when_no_pages() {
    let allocator = Box::leak(Box::new(MockFrameAllocator::empty()));
    let manager = RegionManager::new(allocator, 0);

    let result = manager.allocate_pages(10);
    assert!(result.is_none(), "should return None when no pages available");
}

#[test]
fn allocate_pages_returns_partial_count() {
    let base_pa = 0x4000_0000;
    // Аллокатор может выделить максимум 5 страниц за раз
    let allocator = Box::leak(Box::new(MockFrameAllocator::new(base_pa, 100, 5)));
    let manager = RegionManager::new(allocator, 0);

    // Запрашиваем 20, получаем только 5
    let result = manager.allocate_pages(20);
    assert!(result.is_some());

    let (va, count) = result.unwrap();
    assert_eq!(count, 5, "should return limited count from allocator");
    assert_eq!(va.as_usize(), base_pa);
}

#[test]
fn allocate_pages_zero_returns_none() {
    let allocator = Box::leak(Box::new(MockFrameAllocator::new(0x1000_0000, 100, 100)));
    let manager = RegionManager::new(allocator, 0);

    let result = manager.allocate_pages(0);
    assert!(result.is_none(), "allocating 0 pages should return None");
}

#[test]
fn multiple_allocations_return_sequential_addresses() {
    let base_pa = 0x2000_0000;
    let higher_half_base = 0x1000_0000_0000;

    let allocator = Box::leak(Box::new(MockFrameAllocator::new(base_pa, 100, 10)));
    let manager = RegionManager::new(allocator, higher_half_base);

    // Первое выделение
    let (va1, count1) = manager.allocate_pages(10).unwrap();
    assert_eq!(count1, 10);
    assert_eq!(va1.as_usize(), base_pa + higher_half_base);

    // Второе выделение — должно быть после первого
    let (va2, count2) = manager.allocate_pages(10).unwrap();
    assert_eq!(count2, 10);
    let expected_va2 = base_pa + 10 * PAGE_SIZE + higher_half_base;
    assert_eq!(va2.as_usize(), expected_va2);
}

#[test]
fn allocate_pages_exhausts_available_memory() {
    let base_pa = 0x3000_0000;
    // Всего 3 страницы
    let allocator = Box::leak(Box::new(MockFrameAllocator::new(base_pa, 3, 100)));
    let manager = RegionManager::new(allocator, 0);

    // Первое выделение — получаем все 3
    let result1 = manager.allocate_pages(10);
    assert!(result1.is_some());
    let (_, count1) = result1.unwrap();
    assert_eq!(count1, 3, "should get all available pages");

    // Второе выделение — память исчерпана
    let result2 = manager.allocate_pages(1);
    assert!(result2.is_none(), "should return None when exhausted");
}
