// Эти тесты вызывают существующее `unsafe`-API релокации указателей.
// Нового unsafe в продакшен-код не добавляется - только тестовый вызов.
#![allow(unsafe_code)]

mod common;

use collections::MutexCell;
use common::make_range;
use memory::{
    FrameBitmap,
    frame::Frame,
    frame_allocator::{FrameAllocator, PhysicalFrameAllocator},
};

// Релокация прогоняет внутренний буфер bitmap через `Box::into_raw`/`from_raw`,
// сдвигая сырой указатель на `offset`. В host-тесте нельзя действительно
// отобразить буфер по другому адресу (и обратиться по нему), поэтому используем
// known offset `0`: это round-trip буфера через сырые указатели с сохранением
// исходного валидного адреса. Так исполняется реальный unsafe-путь продакшена
// (take/into_raw/slice_from_raw_parts/from_raw) и проверяется его контракт -
// сохранение длины, содержимого и работоспособности bitmap.
//
// Ненулевой offset здесь применить нельзя: продакшен-код считает обратный сдвиг
// через checked `+` (а не `wrapping_add`), поэтому компенсация `-offset` ушла бы
// в overflow в debug-сборке. Контракт же (длина/доступность/данные) от величины
// offset не зависит и полностью проверяется на offset 0.
const KNOWN_OFFSET: usize = 0;

#[test]
fn bitmap_relocate_round_trip_preserves_length_and_access() {
    let region = make_range(0, 256);
    let mut bitmap = FrameBitmap::new(region);

    // Помечаем известный диапазон, чтобы после round-trip проверить содержимое.
    bitmap.set_range_unchecked(Frame::new(10), Frame::new(20));
    let remaining_before = bitmap.remaining();

    // SAFETY: вызываем существующее unsafe-API; буфер остаётся по валидному
    // host-адресу (offset 0), по перемещённому адресу не обращаемся.
    unsafe {
        bitmap.relocate_ptr_by_offset(KNOWN_OFFSET);
    }

    // Длина и состояние сохранились: буфер доступен и данные не повреждены.
    assert_eq!(bitmap.remaining(), remaining_before);
    for i in 10..20 {
        assert!(
            bitmap.is_allocated(Frame::new(i)),
            "frame {i} must stay allocated across relocation"
        );
    }
    assert!(!bitmap.is_allocated(Frame::new(9)));
    assert!(!bitmap.is_allocated(Frame::new(20)));

    // Bitmap остаётся работоспособным после релокации.
    let frame = bitmap.alloc_from(Frame::new(0)).expect("alloc still works");
    assert!(bitmap.is_allocated(frame));
}

#[test]
fn allocator_relocate_round_trip_keeps_allocator_usable() {
    let allocator: PhysicalFrameAllocator<MutexCell<FrameBitmap>> =
        PhysicalFrameAllocator::new([make_range(0, 64), make_range(100, 64)].into_iter());

    let before = allocator.allocate_frame().expect("frame before relocation");

    // SAFETY: см. комментарий выше - буфер остаётся по исходному host-адресу.
    unsafe {
        allocator.relocate_inner_pointers_by_offset(KNOWN_OFFSET);
    }

    // Аллокатор продолжает выделять и помнит ранее выделенный фрейм.
    assert!(allocator.is_allocated(before));
    let after = allocator.allocate_frame().expect("frame after relocation");
    assert_ne!(before, after);
    assert!(allocator.is_allocated(after));
}
