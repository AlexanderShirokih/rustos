#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used, clippy::std_instead_of_core)]

mod common;

use std::collections::HashSet;

use collections::{MutexCell, NoLockCell};
use common::{make_range, nz};
use memory::{
    FrameBitmap,
    frame::Frame,
    frame_allocator::{
        FrameAllocator, FrameAllocatorInitError, FrameError, MAX_REGIONS, PhysicalFrameAllocator,
        ReserveFrameError,
    },
};

// =============================================================================
// Вспомогательные функции
// =============================================================================

fn single_region_allocator(
    start_frame: usize,
    frame_count: usize,
) -> PhysicalFrameAllocator<MutexCell<FrameBitmap>> {
    let region = make_range(start_frame, frame_count);
    PhysicalFrameAllocator::new([region].into_iter()).expect("single region is valid")
}

fn multi_region_allocator(
    regions: &[(usize, usize)],
) -> PhysicalFrameAllocator<MutexCell<FrameBitmap>> {
    let ranges: Vec<_> = regions
        .iter()
        .map(|(start, count)| make_range(*start, *count))
        .collect();
    PhysicalFrameAllocator::new(ranges.into_iter()).expect("regions are valid")
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

    // Фрейм 0 зарезервирован (один регион начинается с 0), остальное доступно.
    let total_frames = 32 + 64 + 16;
    let reserved_zero = 1;
    assert_eq!(allocated.len(), total_frames - reserved_zero);
}

#[test]
fn new_rejects_empty_regions() {
    let result: Result<PhysicalFrameAllocator<MutexCell<FrameBitmap>>, _> =
        PhysicalFrameAllocator::new(std::iter::empty());
    assert_eq!(result.err(), Some(FrameAllocatorInitError::NoRegions));
}

#[test]
fn new_rejects_more_regions_than_capacity() {
    let ranges: Vec<_> = (0..=MAX_REGIONS).map(|i| make_range(i * 64, 32)).collect();
    let result: Result<PhysicalFrameAllocator<MutexCell<FrameBitmap>>, _> =
        PhysicalFrameAllocator::new(ranges.into_iter());
    assert_eq!(result.err(), Some(FrameAllocatorInitError::TooManyRegions));
}

#[test]
fn frame_zero_is_reserved_automatically() {
    // Регион начинается с 0, но фрейм 0 должен быть зарезервирован
    let allocator = single_region_allocator(0, 16);

    // Фрейм 0 помечен как allocated (зарезервирован)
    assert!(
        allocator.is_allocated(Frame::new(0)),
        "frame 0 should be reserved automatically to keep 0x0 as invalid pointer"
    );

    // Все аллокации должны возвращать фреймы != 0
    for _ in 0..15 {
        let frame = allocator.allocate_frame().unwrap();
        assert_ne!(frame.number(), 0, "frame 0 should never be allocated");
    }

    // После выделения всех 15 фреймов (1-15) следующая аллокация возвращает None
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

    // Фрейм 0 зарезервирован, в первом регионе остаётся 3 фрейма
    assert_eq!(first_region_frames.len(), 3, "should exhaust first region");
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

    // Повторное освобождение возвращает ошибку NotAllocated
    let err = allocator.deallocate_frame(frame).unwrap_err();
    assert!(
        matches!(err, FrameError::NotAllocated),
        "expected NotAllocated, got {err:?}"
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
        "expected NotAllocated for never-allocated frame, got {err:?}"
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

    // Выделяем фреймы из обоих регионов (фрейм 0 зарезервирован)
    let mut frames = Vec::new();
    for _ in 0..15 {
        frames.push(allocator.allocate_frame().unwrap());
    }

    // Должны быть фреймы из обоих регионов
    let first_region: Vec<_> = frames.iter().filter(|f| f.number() < 100).collect();
    let second_region: Vec<_> = frames.iter().filter(|f| f.number() >= 100).collect();

    assert_eq!(first_region.len(), 7);
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
            "reserved frame {i} should not be allocated"
        );
    }

    // Всего фреймов минус зарезервированный диапазон [104,108) и фрейм 0.
    let total_frames = 16 + 16;
    let reserved_range = 108 - 104;
    let reserved_zero = 1;
    assert_eq!(
        allocated.len(),
        total_frames - reserved_range - reserved_zero
    );
}

// =============================================================================
// 6. allocate_frames() - выделение нескольких смежных фреймов
// =============================================================================

#[test]
fn allocate_frames_skips_a_region_without_a_long_enough_run() {
    // В первом регионе свободен только фрейм 3, второй вмещает весь участок.
    let allocator = multi_region_allocator(&[(0, 4), (100, 32)]);
    allocator
        .reserve_frames_exact(Frame::new(1), Frame::new(3))
        .expect("reserve [1,3)");

    let first_frame = allocator
        .allocate_frames(nz(8))
        .expect("second region provides the run");

    assert!(
        first_frame.number() >= 100,
        "run must come from the second region, got frame {}",
        first_frame.number()
    );
    assert!(!allocator.is_allocated(Frame::new(3)));
}

#[test]
fn allocate_frames_full_region_excluding_reserved_zero() {
    let allocator = single_region_allocator(0, 16);

    let first_frame = allocator.allocate_frames(nz(15));
    let next = allocator.allocate_frames(nz(1));

    assert_eq!(first_frame, Some(Frame::new(1)));
    assert!(next.is_none());
}

// =============================================================================
// 7. Граничные случаи deallocate и is_allocated
// =============================================================================

#[test]
fn deallocate_frame_outside_regions_returns_out_of_range() {
    // Регион только 0-15
    let allocator = single_region_allocator(0, 16);

    // Пытаемся освободить фрейм вне управляемого диапазона
    let outside_frame = Frame::new(1000);
    let result = allocator.deallocate_frame(outside_frame);

    assert!(result.is_err(), "deallocate outside regions should fail");
    match result.unwrap_err() {
        FrameError::OutOfRange => {}
        err @ FrameError::NotAllocated => panic!("expected OutOfRange, got {err:?}"),
    }
}

#[test]
fn is_allocated_returns_false_for_frame_outside_regions() {
    // Регион только 0-15
    let allocator = single_region_allocator(0, 16);

    // Фрейм вне управляемого диапазона
    let outside_frame = Frame::new(500);
    assert!(
        !allocator.is_allocated(outside_frame),
        "is_allocated should return false for frame outside managed regions"
    );

    // Ещё один фрейм далеко за пределами
    let far_outside = Frame::new(1_000_000);
    assert!(
        !allocator.is_allocated(far_outside),
        "is_allocated should return false for frame far outside managed regions"
    );
}

#[test]
fn drain_of_multi_region_allocator_stays_inside_declared_regions() {
    // Оба региона оставляют хвостовые биты в последних словах bitmap.
    let regions = [(1usize, 100usize), (500, 65)];
    let allocator = multi_region_allocator(&regions);

    let mut issued = Vec::new();
    while let Some(frame) = allocator.allocate_frame() {
        let number = frame.number();
        assert!(
            regions
                .iter()
                .any(|(start, count)| (*start..start + count).contains(&number)),
            "frame {number} is outside the declared regions"
        );
        issued.push(frame);
        assert!(
            issued.len() <= 165,
            "allocator issued more frames than the regions hold"
        );
    }

    assert_eq!(issued.len(), 165);
}

// =============================================================================
// Смена стратегии блокировки
// =============================================================================

#[test]
fn into_locked_preserves_allocation_state() {
    let region = make_range(1, 64);
    let unlocked: PhysicalFrameAllocator<NoLockCell<FrameBitmap>> =
        PhysicalFrameAllocator::new([region].into_iter()).expect("single region is valid");

    let first = unlocked.allocate_frame().expect("region has free frames");
    let second = unlocked.allocate_frame().expect("region has free frames");
    unlocked
        .deallocate_frame(first)
        .expect("frame is allocated");

    let locked: PhysicalFrameAllocator<MutexCell<FrameBitmap>> = unlocked.into_locked();

    assert!(!locked.is_allocated(first), "the released frame is free");
    assert!(locked.is_allocated(second), "the issued frame stays in use");
}

#[test]
fn into_locked_keeps_regions_and_continues_allocation() {
    let regions = [(1usize, 32usize), (100, 32)];
    let unlocked: PhysicalFrameAllocator<NoLockCell<FrameBitmap>> = PhysicalFrameAllocator::new(
        regions
            .iter()
            .map(|(start, count)| make_range(*start, *count)),
    )
    .expect("regions are valid");

    let issued: HashSet<usize> = core::iter::from_fn(|| unlocked.allocate_frame())
        .take(40)
        .map(|frame| frame.number())
        .collect();

    let locked: PhysicalFrameAllocator<MutexCell<FrameBitmap>> = unlocked.into_locked();

    let mut remaining = 0;
    while let Some(frame) = locked.allocate_frame() {
        assert!(
            !issued.contains(&frame.number()),
            "frame {} is issued twice across the strategy change",
            frame.number()
        );
        remaining += 1;
    }

    assert_eq!(
        issued.len() + remaining,
        64,
        "both regions survive the strategy change"
    );
}
