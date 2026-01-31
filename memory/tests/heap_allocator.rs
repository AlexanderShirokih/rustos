mod common;

use core::alloc::Layout;
use core::sync::atomic::{AtomicUsize, Ordering};
use memory::frame::Frame;
use memory::frame_allocator::{FrameAllocator, FrameError, ReserveFrameError};
use memory::heap_allocator::{AllocationError, HeapAllocator};
use memory::physical_address::PageAlignedAddress;
use memory::region_manager::RegionManager;

// =============================================================================
// Мок-реализация FrameAllocator для тестов
// =============================================================================

const PAGE_SIZE: usize = 4096;

/// Мок-аллокатор фреймов для тестов.
/// Выделяет "физические страницы" из заранее выделенного буфера.
struct MockFrameAllocator {
    /// Начало буфера (имитирует физический адрес)
    base_addr: usize,
    /// Общее количество страниц
    total_pages: usize,
    /// Следующая свободная страница
    next_page: AtomicUsize,
}

impl MockFrameAllocator {
    fn new(buffer: &[u8]) -> Self {
        let base_addr = buffer.as_ptr() as usize;
        // Выравниваем начало на границу страницы
        let aligned_base = (base_addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let usable_size = buffer.len() - (aligned_base - base_addr);
        let total_pages = usable_size / PAGE_SIZE;

        Self {
            base_addr: aligned_base,
            total_pages,
            next_page: AtomicUsize::new(0),
        }
    }
}

impl FrameAllocator for MockFrameAllocator {
    fn reserve_frames_exact(
        &self,
        _from_inclusive: Frame,
        _to_exclusive: Frame,
    ) -> Result<Frame, ReserveFrameError> {
        Ok(_from_inclusive)
    }

    fn allocate_frame(&self) -> Option<Frame> {
        let page = self.next_page.fetch_add(1, Ordering::Relaxed);
        if page >= self.total_pages {
            self.next_page.fetch_sub(1, Ordering::Relaxed);
            return None;
        }
        let addr = self.base_addr + page * PAGE_SIZE;
        Some(Frame::from(
            PageAlignedAddress::from_usize(addr).expect("address should be page aligned"),
        ))
    }

    fn allocate_pages(&self, max_count: usize) -> Option<(Frame, usize)> {
        if max_count == 0 {
            return None;
        }

        let current = self.next_page.load(Ordering::Relaxed);
        let remaining = self.total_pages.saturating_sub(current);
        if remaining == 0 {
            return None;
        }

        let count = max_count.min(remaining);
        let start_page = self.next_page.fetch_add(count, Ordering::Relaxed);

        // Проверка на гонку
        if start_page >= self.total_pages {
            self.next_page.fetch_sub(count, Ordering::Relaxed);
            return None;
        }

        let actual_count = count.min(self.total_pages - start_page);
        let addr = self.base_addr + start_page * PAGE_SIZE;

        Some((
            Frame::from(PageAlignedAddress::from_usize(addr).expect("address should be page aligned")),
            actual_count,
        ))
    }

    fn deallocate_frame(&self, _frame: Frame) -> Result<(), FrameError> {
        // В моке не реализуем освобождение
        Ok(())
    }

    fn is_allocated(&self, _frame: Frame) -> bool {
        false
    }
}

// =============================================================================
// Вспомогательные функции
// =============================================================================

const TEST_HEAP_SIZE: usize = 64 * 1024 + PAGE_SIZE; // 64 KB + padding для выравнивания

/// Выделяет буфер памяти для тестов (leak - память не освобождается)
fn allocate_test_buffer(size: usize) -> &'static mut [u8] {
    Box::leak(vec![0u8; size].into_boxed_slice())
}

/// Создаёт инфраструктуру для тестирования HeapAllocator
fn create_test_allocator() -> HeapAllocator {
    let buffer = allocate_test_buffer(TEST_HEAP_SIZE);
    let frame_allocator: &'static dyn FrameAllocator =
        Box::leak(Box::new(MockFrameAllocator::new(buffer)));

    // higher_half_base = 0, так как мы работаем с реальными адресами буфера
    let region_manager = RegionManager::new(frame_allocator, 0);
    let region_manager: &'static RegionManager = Box::leak(Box::new(region_manager));

    HeapAllocator::new(region_manager)
}

// =============================================================================
// 1. Базовое выделение
// =============================================================================

#[test]
fn allocate_returns_valid_pointer() {
    let mut allocator = create_test_allocator();

    let layout = Layout::from_size_align(64, 8).unwrap();
    let result = allocator.allocate(layout);

    assert!(result.is_ok(), "allocation should succeed");
    let ptr = result.unwrap();

    // Проверяем, что указатель не нулевой
    assert!(!ptr.as_ptr().is_null(), "pointer should not be null");
}

#[test]
fn allocate_returns_aligned_pointer() {
    let mut allocator = create_test_allocator();

    // Тестируем разные выравнивания
    for align in [8, 16, 32, 64, 128] {
        let layout = Layout::from_size_align(32, align).unwrap();
        let ptr = allocator.allocate(layout).unwrap();

        assert_eq!(
            ptr.as_ptr() as usize % align,
            0,
            "pointer should be aligned to {} bytes",
            align
        );
    }
}

#[test]
fn multiple_allocations_return_different_pointers() {
    let mut allocator = create_test_allocator();

    let layout = Layout::from_size_align(64, 8).unwrap();

    let ptr1 = allocator.allocate(layout).unwrap();
    let ptr2 = allocator.allocate(layout).unwrap();
    let ptr3 = allocator.allocate(layout).unwrap();

    assert_ne!(ptr1.as_ptr(), ptr2.as_ptr(), "allocations should be unique");
    assert_ne!(ptr2.as_ptr(), ptr3.as_ptr(), "allocations should be unique");
    assert_ne!(ptr1.as_ptr(), ptr3.as_ptr(), "allocations should be unique");
}

#[test]
fn allocated_memory_is_writable() {
    let mut allocator = create_test_allocator();

    let layout = Layout::from_size_align(128, 8).unwrap();
    let ptr = allocator.allocate(layout).unwrap();

    // Записываем и читаем данные
    unsafe {
        let slice = core::slice::from_raw_parts_mut(ptr.as_ptr(), 128);
        for (i, byte) in slice.iter_mut().enumerate() {
            *byte = (i % 256) as u8;
        }

        // Проверяем, что данные записались корректно
        for (i, byte) in slice.iter().enumerate() {
            assert_eq!(*byte, (i % 256) as u8, "memory should be readable/writable");
        }
    }
}

// =============================================================================
// 2. Обработка ошибок
// =============================================================================

#[test]
fn allocate_zero_size_returns_invalid_layout() {
    let mut allocator = create_test_allocator();

    let layout = Layout::from_size_align(0, 1).unwrap();
    let result = allocator.allocate(layout);

    assert!(
        matches!(result, Err(AllocationError::InvalidLayout)),
        "zero-size allocation should return InvalidLayout, got {:?}",
        result
    );
}

#[test]
fn exhaust_heap_returns_out_of_memory() {
    let mut allocator = create_test_allocator();

    let layout = Layout::from_size_align(8192, 8).unwrap();

    // Выделяем, пока не кончится память
    let mut allocations = 0;
    loop {
        match allocator.allocate(layout) {
            Ok(_) => allocations += 1,
            Err(AllocationError::OutOfMemory) => break,
            Err(e) => panic!("unexpected error: {:?}", e),
        }

        // Защита от бесконечного цикла
        if allocations > 100 {
            panic!("too many allocations, heap should have been exhausted");
        }
    }

    assert!(
        allocations > 0,
        "should have made at least one allocation before OOM"
    );
}

// =============================================================================
// 3. Освобождение и повторное использование
// =============================================================================

#[test]
fn deallocate_allows_reuse() {
    let mut allocator = create_test_allocator();

    let layout = Layout::from_size_align(1024, 8).unwrap();

    // Выделяем блок
    let ptr1 = allocator.allocate(layout).unwrap();

    // Освобождаем
    allocator.deallocate(ptr1);

    // Выделяем снова - должно переиспользовать освобождённую память
    let ptr2 = allocator.allocate(layout).unwrap();

    assert!(!ptr2.as_ptr().is_null(), "should be able to allocate after deallocation");
}

#[test]
fn deallocate_multiple_blocks_allows_reuse() {
    let mut allocator = create_test_allocator();

    let layout = Layout::from_size_align(512, 8).unwrap();

    // Выделяем несколько блоков
    let ptr1 = allocator.allocate(layout).unwrap();
    let ptr2 = allocator.allocate(layout).unwrap();
    let ptr3 = allocator.allocate(layout).unwrap();

    // Освобождаем в обратном порядке
    allocator.deallocate(ptr3);
    allocator.deallocate(ptr2);
    allocator.deallocate(ptr1);

    // Выделяем снова - все блоки должны быть переиспользованы
    let new_ptr1 = allocator.allocate(layout).unwrap();
    let new_ptr2 = allocator.allocate(layout).unwrap();
    let new_ptr3 = allocator.allocate(layout).unwrap();

    // Проверяем, что все указатели разные
    assert_ne!(new_ptr1.as_ptr(), new_ptr2.as_ptr());
    assert_ne!(new_ptr2.as_ptr(), new_ptr3.as_ptr());
    assert_ne!(new_ptr1.as_ptr(), new_ptr3.as_ptr());
}

#[test]
fn deallocate_middle_block_allows_reuse() {
    let mut allocator = create_test_allocator();

    let layout = Layout::from_size_align(256, 8).unwrap();

    // Выделяем три блока
    let ptr1 = allocator.allocate(layout).unwrap();
    let ptr2 = allocator.allocate(layout).unwrap();
    let ptr3 = allocator.allocate(layout).unwrap();

    // Освобождаем средний блок
    allocator.deallocate(ptr2);

    // Выделяем новый блок того же размера - должен занять освобождённое место
    let new_ptr = allocator.allocate(layout).unwrap();

    // Проверяем, что первый и третий блоки не изменились
    assert_ne!(
        new_ptr.as_ptr(),
        ptr1.as_ptr(),
        "new allocation should not overlap with first"
    );
    assert_ne!(
        new_ptr.as_ptr(),
        ptr3.as_ptr(),
        "new allocation should not overlap with third"
    );
}

// =============================================================================
// 4. Расширение кучи
// =============================================================================

#[test]
fn heap_expands_when_needed() {
    let mut allocator = create_test_allocator();

    // Выделяем несколько больших блоков, которые потребуют расширения
    let layout = Layout::from_size_align(8192, 8).unwrap();

    let ptr1 = allocator.allocate(layout);
    let ptr2 = allocator.allocate(layout);
    let ptr3 = allocator.allocate(layout);

    assert!(ptr1.is_ok(), "first large allocation should succeed");
    assert!(ptr2.is_ok(), "second large allocation should succeed");
    assert!(ptr3.is_ok(), "third large allocation should succeed");
}

#[test]
fn varying_sizes_work_correctly() {
    let mut allocator = create_test_allocator();

    // Выделяем блоки разных размеров
    let sizes = [16, 64, 128, 32, 256, 48, 512];

    for &size in &sizes {
        let layout = Layout::from_size_align(size, 8).unwrap();
        let result = allocator.allocate(layout);
        assert!(
            result.is_ok(),
            "allocation of {} bytes should succeed",
            size
        );
    }
}

// =============================================================================
// 5. Тесты на потенциальные баги
// =============================================================================

/// Проверяем, что double-free безопасно игнорируется
#[test]
fn double_free_is_safely_ignored() {
    let mut allocator = create_test_allocator();

    let layout = Layout::from_size_align(64, 8).unwrap();

    // Выделяем блок
    let ptr1 = allocator.allocate(layout).unwrap();

    // Double free - освобождаем дважды
    allocator.deallocate(ptr1);
    allocator.deallocate(ptr1); // Второй вызов должен быть безопасно проигнорирован

    // Два выделения должны вернуть РАЗНЫЕ указатели
    let ptr2 = allocator.allocate(layout).unwrap();
    let ptr3 = allocator.allocate(layout).unwrap();

    assert_ne!(
        ptr2.as_ptr(),
        ptr3.as_ptr(),
        "double free should not cause duplicate allocations"
    );
}

/// Проверяем, что данные в разных блоках не перекрываются
#[test]
fn allocated_blocks_do_not_overlap() {
    let mut allocator = create_test_allocator();

    let layout = Layout::from_size_align(128, 8).unwrap();

    // Выделяем три блока
    let ptr1 = allocator.allocate(layout).unwrap();
    let ptr2 = allocator.allocate(layout).unwrap();
    let ptr3 = allocator.allocate(layout).unwrap();

    // Записываем разные паттерны в каждый блок
    unsafe {
        core::ptr::write_bytes(ptr1.as_ptr(), 0xAA, 128);
        core::ptr::write_bytes(ptr2.as_ptr(), 0xBB, 128);
        core::ptr::write_bytes(ptr3.as_ptr(), 0xCC, 128);
    }

    // Проверяем, что данные не перезаписались
    unsafe {
        let slice1 = core::slice::from_raw_parts(ptr1.as_ptr(), 128);
        let slice2 = core::slice::from_raw_parts(ptr2.as_ptr(), 128);
        let slice3 = core::slice::from_raw_parts(ptr3.as_ptr(), 128);

        assert!(
            slice1.iter().all(|&b| b == 0xAA),
            "block 1 data was corrupted"
        );
        assert!(
            slice2.iter().all(|&b| b == 0xBB),
            "block 2 data was corrupted"
        );
        assert!(
            slice3.iter().all(|&b| b == 0xCC),
            "block 3 data was corrupted"
        );
    }
}

/// Проверяем корректность при большом выравнивании
#[test]
fn large_alignment_works_correctly() {
    let mut allocator = create_test_allocator();

    // Большое выравнивание
    let layout = Layout::from_size_align(64, 256).unwrap();
    let ptr = allocator.allocate(layout).unwrap();

    assert_eq!(
        ptr.as_ptr() as usize % 256,
        0,
        "pointer should be aligned to 256 bytes"
    );

    // Проверяем, что можно записать данные
    unsafe {
        core::ptr::write_bytes(ptr.as_ptr(), 0xFF, 64);
        let slice = core::slice::from_raw_parts(ptr.as_ptr(), 64);
        assert!(slice.iter().all(|&b| b == 0xFF), "memory should be writable");
    }
}

/// Тест на освобождение и повторное выделение с записью данных
#[test]
fn reused_memory_is_independent() {
    let mut allocator = create_test_allocator();

    let layout = Layout::from_size_align(256, 8).unwrap();

    // Выделяем и заполняем данными
    let ptr1 = allocator.allocate(layout).unwrap();
    unsafe {
        core::ptr::write_bytes(ptr1.as_ptr(), 0xAA, 256);
    }

    // Освобождаем
    allocator.deallocate(ptr1);

    // Выделяем снова (может быть тот же блок)
    let ptr2 = allocator.allocate(layout).unwrap();

    // Записываем другой паттерн
    unsafe {
        core::ptr::write_bytes(ptr2.as_ptr(), 0xBB, 256);
    }

    // Выделяем ещё один блок
    let ptr3 = allocator.allocate(layout).unwrap();
    unsafe {
        core::ptr::write_bytes(ptr3.as_ptr(), 0xCC, 256);
    }

    // Проверяем, что данные не перезаписались
    unsafe {
        let slice2 = core::slice::from_raw_parts(ptr2.as_ptr(), 256);
        let slice3 = core::slice::from_raw_parts(ptr3.as_ptr(), 256);

        assert!(
            slice2.iter().all(|&b| b == 0xBB),
            "reused block data was corrupted"
        );
        assert!(
            slice3.iter().all(|&b| b == 0xCC),
            "new block data was corrupted"
        );
    }
}

/// Тест на много мелких аллокаций
#[test]
fn many_small_allocations() {
    let mut allocator = create_test_allocator();

    let layout = Layout::from_size_align(16, 8).unwrap();
    let mut pointers = std::vec::Vec::new();

    // Выделяем много мелких блоков
    for i in 0..100 {
        match allocator.allocate(layout) {
            Ok(ptr) => {
                // Записываем уникальный паттерн
                unsafe {
                    core::ptr::write_bytes(ptr.as_ptr(), i as u8, 16);
                }
                pointers.push((ptr, i as u8));
            }
            Err(AllocationError::OutOfMemory) => break,
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }

    assert!(pointers.len() > 10, "should allocate at least 10 blocks");

    // Проверяем, что все данные сохранились
    for (ptr, pattern) in &pointers {
        unsafe {
            let slice = core::slice::from_raw_parts(ptr.as_ptr(), 16);
            assert!(
                slice.iter().all(|&b| b == *pattern),
                "block with pattern {} was corrupted",
                pattern
            );
        }
    }
}
