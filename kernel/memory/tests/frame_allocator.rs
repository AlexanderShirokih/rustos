mod common;

use std::collections::HashSet;

use collections::MutexCell;
use common::make_range;
use memory::{
    FrameBitmap,
    frame::Frame,
    frame_allocator::{FrameAllocator, FrameError, PhysicalFrameAllocator, ReserveFrameError},
};

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

    // Фрейм 0 зарезервирован (один регион начинается с 0), остальное доступно.
    let total_frames = 32 + 64 + 16;
    let reserved_zero = 1;
    assert_eq!(allocated.len(), total_frames - reserved_zero);
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
fn allocate_frames_returns_contiguous_frames() {
    let allocator = single_region_allocator(0, 64);

    // Запрашиваем 8 смежных фреймов
    let result = allocator.allocate_frames(8);
    assert!(result.is_some(), "should allocate 8 contiguous frames");

    let (first_frame, count) = result.unwrap();
    assert_eq!(count, 8, "should return exactly 8 frames");

    // Проверка, что все фреймы помечены как allocated
    for i in 0..8 {
        let frame = Frame::new(first_frame.number() + i);
        assert!(
            allocator.is_allocated(frame),
            "frame {} should be allocated",
            frame.number()
        );
    }

    // Следующие фреймы не должны быть затронуты
    let next_frame = Frame::new(first_frame.number() + 8);
    assert!(
        !allocator.is_allocated(next_frame),
        "frame after allocated range should be free"
    );
}

#[test]
fn allocate_frames_returns_less_when_not_enough() {
    // Регион с 10 фреймами (фрейм 0 зарезервирован, остаётся 9)
    let allocator = single_region_allocator(0, 10);

    // Запрашиваем 100 фреймов, но доступно только 9
    let result = allocator.allocate_frames(100);
    assert!(result.is_some(), "should return available frames");

    let (first_frame, count) = result.unwrap();
    // Должно вернуть максимум 9 (10 - зарезервированный фрейм 0)
    assert!(
        count <= 9,
        "should return at most 9 frames (10 - reserved frame 0), got {count}"
    );
    assert!(count > 0, "should return at least some frames");

    // Проверяем, что фреймы действительно выделены
    for i in 0..count {
        let frame = Frame::new(first_frame.number() + i);
        assert!(
            allocator.is_allocated(frame),
            "frame {} should be allocated",
            frame.number()
        );
    }
}

#[test]
fn allocate_frames_returns_none_when_exhausted() {
    let allocator = single_region_allocator(0, 16);

    // Выделяем все фреймы по одному
    while allocator.allocate_frame().is_some() {}

    // Теперь allocate_frames возвращает None
    let result = allocator.allocate_frames(4);
    assert!(
        result.is_none(),
        "allocate_frames should return None when all frames exhausted"
    );
}

#[test]
fn allocate_frames_searches_multiple_regions() {
    // Первый регион маленький (4 фрейма), второй большой (32 фрейма)
    let allocator = multi_region_allocator(&[(0, 4), (100, 32)]);

    // Полностью исчерпываем первый регион
    // Фрейм 0 зарезервирован, остаётся 3 фрейма (1, 2, 3)
    for _ in 0..3 {
        allocator.allocate_frame().unwrap();
    }

    // Первый регион исчерпан, allocate_frames ищет во втором
    let result = allocator.allocate_frames(8);
    assert!(
        result.is_some(),
        "should find contiguous frames in second region"
    );

    let (first_frame, count) = result.unwrap();
    assert_eq!(count, 8, "should allocate exactly 8 frames");

    // Фреймы должны быть из второго региона (>= 100)
    assert!(
        first_frame.number() >= 100,
        "frames should come from second region, got frame {}",
        first_frame.number()
    );
}

#[test]
fn allocate_frames_returns_short_run_when_first_free_segment_is_small() {
    // Документирует opportunistic-семантику alloc_contiguous: первая
    // же дырка возвращается целиком, длинный run дальше игнорируется.
    let allocator = single_region_allocator(0, 64);

    // Свободно: только фрейм 5 и хвост [10..20).
    allocator
        .reserve_frames_exact(Frame::new(1), Frame::new(5))
        .expect("reserve [1,5)");
    allocator
        .reserve_frames_exact(Frame::new(6), Frame::new(10))
        .expect("reserve [6,10)");
    allocator
        .reserve_frames_exact(Frame::new(20), Frame::new(64))
        .expect("reserve [20,64)");

    let (first_frame, count) = allocator
        .allocate_frames(5)
        .expect("opportunistic: returns at least 1");
    assert_eq!(first_frame.number(), 5);
    assert_eq!(count, 1);
}

#[test]
fn allocate_frames_skips_exhausted_first_region_for_longer_run() {
    // Opportunistic-семантика между регионами: `allocate_frames` берёт run из
    // первого региона, где он есть. Если первый регион занят целиком, переходит
    // ко второму и возвращает его (потенциально более длинный) run.
    let allocator = multi_region_allocator(&[(0, 4), (100, 32)]);

    // Полностью занимаем первый регион (фрейм 0 уже зарезервирован).
    allocator
        .reserve_frames_exact(Frame::new(1), Frame::new(4))
        .expect("reserve [1,4)");

    let (first_frame, count) = allocator
        .allocate_frames(16)
        .expect("second region provides the run");

    assert!(
        first_frame.number() >= 100,
        "run must come from the second region, got frame {}",
        first_frame.number()
    );
    assert_eq!(count, 16, "second region has room for the full request");
}

#[test]
fn allocate_frames_prefers_first_region_even_with_short_run() {
    // Зеркало предыдущего теста: пока в первом регионе есть хоть один свободный
    // фрейм, run берётся оттуда, даже если второй регион предложил бы больше.
    let allocator = multi_region_allocator(&[(0, 4), (100, 32)]);

    // Оставляем в первом регионе единственную дырку - фрейм 3.
    allocator
        .reserve_frames_exact(Frame::new(1), Frame::new(3))
        .expect("reserve [1,3)");

    let (first_frame, count) = allocator
        .allocate_frames(16)
        .expect("first region still has a frame");

    assert!(
        first_frame.number() < 100,
        "run must come from the first region, got frame {}",
        first_frame.number()
    );
    assert_eq!(count, 1, "only one frame free in the first region");
}

#[test]
fn allocate_frames_spans_word_boundary() {
    // 128 фреймов = 2 u64-word'а; проверяем счёт через границу.
    let allocator = single_region_allocator(0, 128);
    let (first_frame, count) = allocator
        .allocate_frames(80)
        .expect("80 contiguous within 128-frame region");
    assert_eq!(count, 80);
    assert!(first_frame.number() + 80 > 64);
}

#[test]
fn allocate_frames_full_region_excluding_reserved_zero() {
    let allocator = single_region_allocator(0, 16);
    let (first_frame, count) = allocator
        .allocate_frames(15)
        .expect("15 free frames available");
    assert_eq!(first_frame.number(), 1);
    assert_eq!(count, 15);
    assert!(allocator.allocate_frames(1).is_none());
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
