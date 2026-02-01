mod common;

use collections::MutexCell;
use common::make_range;
use memory::frame::Frame;
use memory::frame_allocator::{
    FrameAllocator, FrameError, PhysicalFrameAllocator, ReserveFrameError,
};
use memory::FrameBitmap;
use std::collections::HashSet;

// =============================================================================
// Вспомогательные функции
// =============================================================================

fn single_region_allocator(
    start_frame: usize,
    frame_count: usize,
) -> PhysicalFrameAllocator<MutexCell<FrameBitmap>> {
    let region = make_range(start_frame, frame_count);
    PhysicalFrameAllocator::new([region].into_iter())
}

fn multi_region_allocator(
    regions: &[(usize, usize)],
) -> PhysicalFrameAllocator<MutexCell<FrameBitmap>> {
    let ranges: Vec<_> = regions
        .iter()
        .map(|(start, count)| make_range(*start, *count))
        .collect();
    PhysicalFrameAllocator::new(ranges.into_iter())
}

// =============================================================================
// 1. Создание и инициализация
// =============================================================================

#[test]
fn new_with_multiple_regions_succeeds() {
    let allocator = multi_region_allocator(&[(0, 32), (100, 64), (200, 16)]);

    // Проверяем, что можем выделять фреймы из всех регионов
    let mut allocated = HashSet::new();
    while let Some(frame) = allocator.allocate_frame() {
        allocated.insert(frame.number());
    }

    // Должно быть выделено 32 + 64 + 16 = 112 фреймов
    assert_eq!(allocated.len(), 112);
}

#[test]
#[should_panic(expected = "Managed memory regions should not be empty")]
fn new_panics_on_empty_regions() {
    let _: PhysicalFrameAllocator<MutexCell<FrameBitmap>> =
        PhysicalFrameAllocator::new(std::iter::empty());
}

#[test]
fn frame_zero_is_reserved_automatically() {
    // Регион начинается с 0, но фрейм 0 должен быть зарезервирован
    let allocator = single_region_allocator(0, 16);

    // Фрейм 0 должен быть помечен как allocated (зарезервирован)
    assert!(
        allocator.is_allocated(Frame::new(0)),
        "frame 0 should be reserved automatically to keep 0x0 as invalid pointer"
    );

    // Все аллокации должны возвращать фреймы != 0
    for _ in 0..15 {
        let frame = allocator.allocate_frame().unwrap();
        assert_ne!(frame.number(), 0, "frame 0 should never be allocated");
    }

    // После выделения всех 15 фреймов (1-15), следующая аллокация вернёт None
    assert!(allocator.allocate_frame().is_none());
}

// =============================================================================
// 2. Аллокация фреймов
// =============================================================================

#[test]
fn allocate_switches_to_next_region_when_exhausted() {
    // Первый регион маленький (4 фрейма), второй большой (32 фрейма)
    let allocator = multi_region_allocator(&[(0, 4), (100, 32)]);

    let mut first_region_frames = Vec::new();
    let mut second_region_frames = Vec::new();

    // Выделяем все 36 фреймов
    while let Some(frame) = allocator.allocate_frame() {
        if frame.number() < 100 {
            first_region_frames.push(frame);
        } else {
            second_region_frames.push(frame);
        }
    }

    assert_eq!(first_region_frames.len(), 4, "should exhaust first region");
    assert_eq!(
        second_region_frames.len(),
        32,
        "should use all of second region"
    );
}

#[test]
fn allocate_uses_hint_and_wraps_around() {
    let allocator = single_region_allocator(0, 16);

    // Выделяем несколько фреймов
    let first = allocator.allocate_frame().unwrap();
    let _second = allocator.allocate_frame().unwrap();
    let _third = allocator.allocate_frame().unwrap();

    // Освобождаем первый фрейм
    allocator.deallocate_frame(first).unwrap();

    // Продолжаем выделять - должны дойти до конца и затем найти освобождённый
    let mut found_recycled = false;
    while let Some(frame) = allocator.allocate_frame() {
        if frame == first {
            found_recycled = true;
            break;
        }
    }

    assert!(
        found_recycled,
        "allocator should wrap around and find freed frame"
    );
}

// =============================================================================
// 3. Деаллокация фреймов
// =============================================================================

#[test]
fn deallocate_same_frame_twice_returns_not_allocated() {
    let allocator = single_region_allocator(0, 16);

    let frame = allocator.allocate_frame().unwrap();

    // Первое освобождение успешно
    assert!(allocator.deallocate_frame(frame).is_ok());

    // Повторное освобождение должно вернуть ошибку NotAllocated
    let err = allocator.deallocate_frame(frame).unwrap_err();
    assert!(
        matches!(err, FrameError::NotAllocated),
        "expected NotAllocated, got {:?}",
        err
    );
}

#[test]
fn deallocate_never_allocated_frame_returns_not_allocated() {
    let allocator = single_region_allocator(0, 16);

    // Фрейм в диапазоне, но никогда не выделялся
    let frame = Frame::new(5);
    let err = allocator.deallocate_frame(frame).unwrap_err();

    assert!(
        matches!(err, FrameError::NotAllocated),
        "expected NotAllocated for never-allocated frame, got {:?}",
        err
    );
}

// =============================================================================
// 4. Резервирование
// =============================================================================

#[test]
fn reserve_spanning_region_boundary_fails() {
    // Два несмежных региона: 0-31 и 100-131
    let allocator = multi_region_allocator(&[(0, 32), (100, 32)]);

    // Пытаемся зарезервировать диапазон, пересекающий границу регионов
    let from = Frame::new(20);
    let to = Frame::new(110);

    let result = allocator.reserve_frames_exact(from, to);

    assert!(
        result.is_err(),
        "reservation spanning region boundary should fail"
    );
    match result.unwrap_err() {
        ReserveFrameError::OutOfTargetBoundary {
            from_inclusive,
            to_exclusive,
        } => {
            assert_eq!(from_inclusive, from);
            assert_eq!(to_exclusive, to);
        }
    }
}

#[test]
fn reserve_in_gap_between_regions_fails() {
    // Регионы: 0-31 и 100-131, промежуток 32-99 не управляется
    let allocator = multi_region_allocator(&[(0, 32), (100, 32)]);

    let from = Frame::new(50);
    let to = Frame::new(60);

    let result = allocator.reserve_frames_exact(from, to);
    assert!(result.is_err(), "reservation in gap should fail");
}

// =============================================================================
// 5. Работа с несколькими регионами
// =============================================================================

#[test]
fn multi_region_allocates_from_first_region_initially() {
    let allocator = multi_region_allocator(&[(10, 8), (100, 8), (200, 8)]);

    // Первые аллокации должны быть из первого региона (фреймы 10-17)
    for _ in 0..8 {
        let frame = allocator.allocate_frame().unwrap();
        assert!(
            frame.number() >= 10 && frame.number() < 18,
            "initial allocations should come from first region, got frame {}",
            frame.number()
        );
    }

    // Следующая аллокация должна быть из второго региона
    let next = allocator.allocate_frame().unwrap();
    assert!(
        next.number() >= 100 && next.number() < 108,
        "after first region exhausted, should allocate from second region, got frame {}",
        next.number()
    );
}

#[test]
fn deallocate_works_across_multiple_regions() {
    let allocator = multi_region_allocator(&[(0, 8), (100, 8)]);

    // Выделяем фреймы из обоих регионов
    let mut frames = Vec::new();
    for _ in 0..16 {
        frames.push(allocator.allocate_frame().unwrap());
    }

    // Должны быть фреймы из обоих регионов
    let first_region: Vec<_> = frames.iter().filter(|f| f.number() < 100).collect();
    let second_region: Vec<_> = frames.iter().filter(|f| f.number() >= 100).collect();

    assert_eq!(first_region.len(), 8);
    assert_eq!(second_region.len(), 8);

    // Освобождаем по одному из каждого региона
    allocator.deallocate_frame(*first_region[3]).unwrap();
    allocator.deallocate_frame(*second_region[5]).unwrap();

    // Можем снова выделить эти фреймы
    let recycled1 = allocator.allocate_frame().unwrap();
    let recycled2 = allocator.allocate_frame().unwrap();

    let recycled_numbers: HashSet<_> = [recycled1.number(), recycled2.number()]
        .into_iter()
        .collect();

    assert!(
        recycled_numbers.contains(&first_region[3].number())
            || recycled_numbers.contains(&second_region[5].number()),
        "freed frames should be recycled"
    );
}

#[test]
fn reserve_in_second_region_works() {
    let allocator = multi_region_allocator(&[(0, 16), (100, 16)]);

    // Резервируем во втором регионе
    let reserve_start = Frame::new(104);
    let reserve_end = Frame::new(108);
    allocator
        .reserve_frames_exact(reserve_start, reserve_end)
        .unwrap();

    // Выделяем все фреймы
    let mut allocated = HashSet::new();
    while let Some(frame) = allocator.allocate_frame() {
        allocated.insert(frame.number());
    }

    // Зарезервированные фреймы не должны быть выделены
    for i in 104..108 {
        assert!(
            !allocated.contains(&i),
            "reserved frame {} should not be allocated",
            i
        );
    }

    // 16 + 16 - 4 = 28 фреймов
    assert_eq!(allocated.len(), 28);
}
