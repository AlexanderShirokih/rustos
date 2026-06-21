mod common;

use common::{frame_to_address, make_range};
use memory::{FrameBitmap, frame::Frame};

#[test]
fn new_bitmap_has_all_frames_free() {
    let region = make_range(0, 128);
    let bitmap = FrameBitmap::new(region);

    assert_eq!(bitmap.remaining(), 128);
}

#[test]
fn new_bitmap_reports_correct_start() {
    let region = make_range(10, 64);
    let bitmap = FrameBitmap::new(region);

    assert_eq!(bitmap.start(), frame_to_address(10));
}

#[test]
fn new_bitmap_reports_correct_range() {
    let region = make_range(5, 32);
    let bitmap = FrameBitmap::new(region);

    let range = bitmap.range();
    assert_eq!(range.start(), frame_to_address(5));
    assert_eq!(range.end(), frame_to_address(5 + 32));
}

#[test]
fn new_bitmap_has_no_allocated_frames() {
    let region = make_range(0, 64);
    let bitmap = FrameBitmap::new(region);

    for i in 0..64 {
        let frame = Frame::new(i);
        assert!(
            !bitmap.is_allocated(frame),
            "frame {i} should not be allocated in new bitmap"
        );
    }
}

#[test]
fn alloc_first_frame_succeeds() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    let frame = bitmap.alloc_from(Frame::new(0));
    assert!(frame.is_some(), "should allocate first frame");
}

#[test]
fn alloc_decreases_remaining() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    assert_eq!(bitmap.remaining(), 64);

    bitmap.alloc_from(Frame::new(0));
    assert_eq!(bitmap.remaining(), 63);

    bitmap.alloc_from(Frame::new(0));
    assert_eq!(bitmap.remaining(), 62);
}

#[test]
fn alloc_marks_frame_as_allocated() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    let frame = bitmap.alloc_from(Frame::new(0)).unwrap();
    assert!(
        bitmap.is_allocated(frame),
        "allocated frame should be marked as allocated"
    );
}

#[test]
fn alloc_returns_different_frames_on_sequential_calls() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    let frame1 = bitmap.alloc_from(Frame::new(0)).unwrap();
    let frame2 = bitmap.alloc_from(Frame::new(0)).unwrap();
    let frame3 = bitmap.alloc_from(Frame::new(0)).unwrap();

    assert_ne!(frame1, frame2);
    assert_ne!(frame2, frame3);
    assert_ne!(frame1, frame3);
}

#[test]
fn alloc_exhausts_all_frames() {
    let region = make_range(0, 16);
    let mut bitmap = FrameBitmap::new(region);

    let mut allocated = Vec::new();
    while let Some(frame) = bitmap.alloc_from(Frame::new(0)) {
        allocated.push(frame);
    }

    assert_eq!(allocated.len(), 16, "should allocate exactly 16 frames");
}

#[test]
fn remaining_is_zero_after_exhaustion() {
    let region = make_range(0, 8);
    let mut bitmap = FrameBitmap::new(region);

    while bitmap.alloc_from(Frame::new(0)).is_some() {}

    assert_eq!(bitmap.remaining(), 0);
}

#[test]
fn alloc_returns_none_when_exhausted() {
    let region = make_range(0, 4);
    let mut bitmap = FrameBitmap::new(region);

    // Выделяем все фреймы
    for _ in 0..4 {
        bitmap.alloc_from(Frame::new(0)).unwrap();
    }

    // Следующая попытка возвращает None
    assert!(bitmap.alloc_from(Frame::new(0)).is_none());
    assert!(bitmap.alloc_from(Frame::new(0)).is_none());
}

#[test]
fn clear_allocated_frame_returns_true() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    let frame = bitmap.alloc_from(Frame::new(0)).unwrap();
    assert!(
        bitmap.clear(frame),
        "clearing allocated frame should return true"
    );
}

#[test]
fn clear_increases_remaining() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    let frame = bitmap.alloc_from(Frame::new(0)).unwrap();
    assert_eq!(bitmap.remaining(), 63);

    bitmap.clear(frame);
    assert_eq!(bitmap.remaining(), 64);
}

#[test]
fn clear_makes_frame_available_for_reallocation() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    let frame = bitmap.alloc_from(Frame::new(0)).unwrap();
    bitmap.clear(frame);

    assert!(
        !bitmap.is_allocated(frame),
        "cleared frame should not be allocated"
    );
}

#[test]
fn clear_already_free_frame_returns_false() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    let frame = Frame::new(5);
    assert!(
        !bitmap.clear(frame),
        "clearing already free frame should return false"
    );
}

#[test]
fn clear_out_of_range_frame_returns_false() {
    let region = make_range(10, 20);
    let mut bitmap = FrameBitmap::new(region);

    // Фрейм до начала региона
    assert!(!bitmap.clear(Frame::new(5)));

    // Фрейм после конца региона
    assert!(!bitmap.clear(Frame::new(100)));
}

#[test]
fn clear_does_not_change_remaining_for_free_frame() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    let initial = bitmap.remaining();
    bitmap.clear(Frame::new(10)); // Фрейм ещё не выделен

    assert_eq!(bitmap.remaining(), initial);
}

#[test]
fn set_range_marks_frames_as_allocated() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    let from = Frame::new(8);
    let to = Frame::new(16);
    bitmap.set_range_unchecked(from, to);

    for i in 8..16 {
        assert!(
            bitmap.is_allocated(Frame::new(i)),
            "frame {i} should be allocated after set_range"
        );
    }

    // Проверяем, что соседние фреймы не затронуты
    assert!(!bitmap.is_allocated(Frame::new(7)));
    assert!(!bitmap.is_allocated(Frame::new(16)));
}

#[test]
fn set_range_updates_remaining_correctly() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    bitmap.set_range_unchecked(Frame::new(10), Frame::new(20));

    assert_eq!(bitmap.remaining(), 64 - 10);
}

#[test]
fn set_range_spanning_multiple_words() {
    let region = make_range(0, 256);
    let mut bitmap = FrameBitmap::new(region);

    // Диапазон пересекает несколько u64 слов (64 бита каждое)
    bitmap.set_range_unchecked(Frame::new(60), Frame::new(130));

    for i in 60..130 {
        assert!(
            bitmap.is_allocated(Frame::new(i)),
            "frame {i} should be allocated"
        );
    }

    assert!(!bitmap.is_allocated(Frame::new(59)));
    assert!(!bitmap.is_allocated(Frame::new(130)));
}

#[test]
fn set_range_is_idempotent() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    bitmap.set_range_unchecked(Frame::new(5), Frame::new(15));
    let remaining_after_first = bitmap.remaining();

    // Повторная установка того же диапазона
    bitmap.set_range_unchecked(Frame::new(5), Frame::new(15));

    assert_eq!(
        bitmap.remaining(),
        remaining_after_first,
        "idempotent set_range should not change remaining"
    );
}

#[test]
fn set_unchecked_updates_remaining_once() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);
    let initial = bitmap.remaining();

    bitmap.set_unchecked(Frame::new(10));
    assert_eq!(bitmap.remaining(), initial - 1);

    // Повторная установка не уменьшает free ещё раз
    bitmap.set_unchecked(Frame::new(10));
    assert_eq!(bitmap.remaining(), initial - 1);
}

#[test]
fn set_range_with_partial_overlap() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    bitmap.set_range_unchecked(Frame::new(10), Frame::new(20));
    assert_eq!(bitmap.remaining(), 54);

    // Частично перекрывающийся диапазон
    bitmap.set_range_unchecked(Frame::new(15), Frame::new(25));
    // Новых фреймов: 20..25 = 5 штук
    assert_eq!(bitmap.remaining(), 49);
}

#[test]
fn alloc_skips_reserved_range() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    bitmap.set_range_unchecked(Frame::new(0), Frame::new(10));

    let frame = bitmap.alloc_from(Frame::new(0)).unwrap();
    assert!(
        frame.number() >= 10,
        "allocated frame should be outside reserved range"
    );
}

#[test]
fn alloc_from_finds_first_free_after_offset() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    // Занимаем первые 10 фреймов
    bitmap.set_range_unchecked(Frame::new(0), Frame::new(10));

    let frame = bitmap.alloc_from(Frame::new(5)).unwrap();
    assert!(
        frame.number() >= 10,
        "should find frame after reserved range"
    );
}

#[test]
fn alloc_from_wraps_to_beginning() {
    let region = make_range(0, 32);
    let mut bitmap = FrameBitmap::new(region);

    // Занимаем фреймы 16..32
    bitmap.set_range_unchecked(Frame::new(16), Frame::new(32));

    // Ищем с позиции 20 - должен найти в начале
    let frame = bitmap.alloc_from(Frame::new(20)).unwrap();
    assert!(
        frame.number() < 16,
        "should wrap around and find frame at beginning"
    );
}

#[test]
fn alloc_from_with_single_free_frame() {
    let region = make_range(0, 32);
    let mut bitmap = FrameBitmap::new(region);

    // Занимаем все кроме одного
    bitmap.set_range_unchecked(Frame::new(0), Frame::new(15));
    bitmap.set_range_unchecked(Frame::new(16), Frame::new(32));

    // Освобождаем фрейм 15
    // Не был занят, так как set_range exclusive
    let frame = bitmap.alloc_from(Frame::new(0)).unwrap();
    assert_eq!(frame.number(), 15);
}

#[test]
fn alloc_from_exhausted_region_returns_none() {
    let region = make_range(0, 16);
    let mut bitmap = FrameBitmap::new(region);

    bitmap.set_range_unchecked(Frame::new(0), Frame::new(16));

    assert!(bitmap.alloc_from(Frame::new(0)).is_none());
    assert!(bitmap.alloc_from(Frame::new(8)).is_none());
}

#[test]
fn cleared_frame_can_be_reallocated_via_wrap() {
    let region = make_range(0, 32);
    let mut bitmap = FrameBitmap::new(region);

    // Выделяем все фреймы
    let mut frames = Vec::new();
    while let Some(f) = bitmap.alloc_from(Frame::new(0)) {
        frames.push(f);
    }

    // Освобождаем первый
    let first = frames[0];
    bitmap.clear(first);

    // Поиск с конца - находит освобождённый через wrap
    let reallocated = bitmap.alloc_from(Frame::from(region.end())).unwrap();
    assert_eq!(reallocated, first);
}

// =============================================================================
// alloc_contiguous - граничные случаи
// =============================================================================

#[test]
fn alloc_contiguous_zero_count_returns_none() {
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    assert!(
        bitmap.alloc_contiguous(0).is_none(),
        "max_count == 0 must allocate nothing"
    );
    // Ничего не выделено.
    assert_eq!(bitmap.remaining(), 64);
}

#[test]
fn alloc_contiguous_on_fully_occupied_non_word_aligned_region_returns_none() {
    // 100 фреймов не кратно 64 (1 полное слово + 36 бит во втором).
    let region = make_range(0, 100);
    let mut bitmap = FrameBitmap::new(region);

    // Занимаем весь регион.
    bitmap.set_range_unchecked(Frame::new(0), Frame::new(100));
    assert_eq!(bitmap.remaining(), 0);

    assert!(
        bitmap.alloc_contiguous(4).is_none(),
        "fully occupied region must yield no contiguous run"
    );
}

#[test]
fn alloc_contiguous_does_not_run_past_region_end() {
    // Регион из 100 фреймов; биты [100..128) во втором слове физически
    // существуют, но лежат за границей региона и не должны выделяться.
    let region = make_range(0, 100);
    let mut bitmap = FrameBitmap::new(region);

    // Оставляем свободным только хвост [90..100) - ровно у границы региона.
    bitmap.set_range_unchecked(Frame::new(0), Frame::new(90));

    let (first, count) = bitmap
        .alloc_contiguous(64)
        .expect("tail must be allocatable");

    assert_eq!(first.number(), 90, "run must start at the first free frame");
    assert_eq!(
        count, 10,
        "run must stop at the region boundary, not bleed into padding bits"
    );

    // Padding-фреймы за границей региона не выделены.
    assert!(!bitmap.is_allocated(Frame::new(100)));
    assert!(!bitmap.is_allocated(Frame::new(110)));
}

#[test]
fn alloc_contiguous_run_exactly_at_region_boundary() {
    // Регион ровно на одно слово (64 фрейма). Просим больше, чем есть -
    // должны получить ровно весь регион, без выхода за его пределы.
    let region = make_range(0, 64);
    let mut bitmap = FrameBitmap::new(region);

    let (first, count) = bitmap
        .alloc_contiguous(128)
        .expect("single-word region must allocate fully");

    assert_eq!(first.number(), 0);
    assert_eq!(count, 64, "must clamp to the region's frame count");
    assert_eq!(bitmap.remaining(), 0);
}

#[test]
fn is_in_range_true_for_frames_inside() {
    let region = make_range(10, 20);
    let bitmap = FrameBitmap::new(region);

    assert!(bitmap.is_in_range(Frame::new(10))); // начало
    assert!(bitmap.is_in_range(Frame::new(15))); // середина
    assert!(bitmap.is_in_range(Frame::new(29))); // конец (10 + 20 - 1)
}

#[test]
fn is_in_range_false_for_frames_outside() {
    let region = make_range(10, 20);
    let bitmap = FrameBitmap::new(region);

    assert!(!bitmap.is_in_range(Frame::new(0))); // до начала
    assert!(!bitmap.is_in_range(Frame::new(9))); // прямо перед началом
    assert!(!bitmap.is_in_range(Frame::new(30))); // сразу после конца
    assert!(!bitmap.is_in_range(Frame::new(100))); // далеко за концом
}

#[test]
fn is_allocated_returns_false_for_out_of_range() {
    let region = make_range(10, 20);
    let bitmap = FrameBitmap::new(region);

    assert!(!bitmap.is_allocated(Frame::new(5)));
    assert!(!bitmap.is_allocated(Frame::new(50)));
}
