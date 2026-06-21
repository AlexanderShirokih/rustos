use core::alloc::Layout;

use memory::{
    aligned::Aligned,
    bump_allocator::{BumpAllocError, BumpAllocator},
    physical_address::PageAlignedAddress,
};

const PAGE: usize = PageAlignedAddress::ALIGNMENT;

fn aligned(addr: usize) -> PageAlignedAddress {
    PageAlignedAddress::from_usize(addr).expect("address must be page-aligned")
}

#[test]
fn allocate_detects_address_overflow() {
    let align = PAGE;
    let base = usize::MAX - (align - 1);
    let start = aligned(base);
    let end = start;

    let mut allocator = BumpAllocator::new(start, end);
    let layout = Layout::from_size_align(align + 1, 8).expect("layout");

    let err = allocator.allocate(layout).unwrap_err();
    assert!(matches!(err, BumpAllocError::AddressOverflow));
}

#[test]
fn allocate_happy_path_advances_offset() {
    // Регион [0x1000, 0x1000 + 4 страницы).
    let mut allocator = BumpAllocator::new(aligned(PAGE), aligned(PAGE * 5));
    assert_eq!(allocator.remaining(), PAGE * 4);

    let layout = Layout::from_size_align(64, 8).expect("layout");
    let ptr = allocator
        .allocate(layout)
        .expect("first allocation succeeds");

    // Возвращённый указатель лежит в начале региона.
    assert_eq!(ptr.as_ptr() as usize, PAGE);
    // offset продвинулся ровно на запрошенный размер.
    assert_eq!(allocator.remaining(), PAGE * 4 - 64);
    assert_eq!(allocator.used().size(), 64);

    // Следующая аллокация идёт сразу после предыдущей (с учётом выравнивания).
    let ptr2 = allocator
        .allocate(layout)
        .expect("second allocation succeeds");
    assert_eq!(ptr2.as_ptr() as usize, PAGE + 64);
}

#[test]
fn allocate_aligns_returned_pointer() {
    // Старт выровнен на страницу, но запрашиваем выравнивание 256 после
    // нечётной аллокации - указатель должен быть подогнан вверх.
    let mut allocator = BumpAllocator::new(aligned(PAGE), aligned(PAGE * 2));

    // Сдвигаем offset на 10 байт.
    let small = Layout::from_size_align(10, 1).expect("layout");
    allocator.allocate(small).expect("small allocation");

    let aligned_layout = Layout::from_size_align(16, 256).expect("layout");
    let ptr = allocator
        .allocate(aligned_layout)
        .expect("aligned allocation");

    let addr = ptr.as_ptr() as usize;
    assert_eq!(addr % 256, 0, "returned pointer must honor requested align");
    // Должен быть подогнан вверх от PAGE+10 до PAGE+256 (PAGE кратен 256).
    assert_eq!(addr, PAGE + 256);
}

#[test]
fn allocate_out_of_memory_reports_required_and_available() {
    // Маленький регион: ровно одна страница.
    let mut allocator = BumpAllocator::new(aligned(PAGE), aligned(PAGE * 2));
    assert_eq!(allocator.remaining(), PAGE);

    // Запрашиваем больше, чем есть.
    let layout = Layout::from_size_align(PAGE + 1, 8).expect("layout");
    let err = allocator.allocate(layout).unwrap_err();

    match err {
        BumpAllocError::OutOfMemory {
            required_size,
            available_size,
        } => {
            assert_eq!(required_size, PAGE + 1);
            assert_eq!(available_size, PAGE);
        }
        other @ BumpAllocError::AddressOverflow => panic!("expected OutOfMemory, got {other}"),
    }

    // Неудачная аллокация не должна была сдвинуть offset.
    assert_eq!(allocator.remaining(), PAGE);
}

#[test]
fn allocate_exact_fit_succeeds_then_exhausted() {
    let mut allocator = BumpAllocator::new(aligned(PAGE), aligned(PAGE * 2));

    let layout = Layout::from_size_align(PAGE, 8).expect("layout");
    allocator.allocate(layout).expect("exact fit succeeds");
    assert_eq!(allocator.remaining(), 0);

    // Любая дальнейшая аллокация - OutOfMemory.
    let one = Layout::from_size_align(1, 1).expect("layout");
    assert!(matches!(
        allocator.allocate(one),
        Err(BumpAllocError::OutOfMemory { .. })
    ));
}

#[test]
fn used_grows_with_allocations() {
    let mut allocator = BumpAllocator::new(aligned(PAGE), aligned(PAGE * 4));
    assert_eq!(allocator.used().size(), 0);

    let layout = Layout::from_size_align(100, 8).expect("layout");
    allocator.allocate(layout).expect("alloc");
    assert_eq!(allocator.used().start().as_usize(), PAGE);
    assert_eq!(allocator.used().size(), 100);
}

#[test]
fn degenerate_range_to_below_from_collapses_to_empty() {
    let allocator = BumpAllocator::new(aligned(PAGE * 4), aligned(PAGE));
    assert_eq!(allocator.remaining(), 0, "degenerate range must be empty");
}

#[test]
fn degenerate_range_rejects_allocation_without_panic() {
    let mut allocator = BumpAllocator::new(aligned(PAGE * 4), aligned(PAGE));
    let layout = Layout::from_size_align(1, 1).expect("layout");
    assert!(matches!(
        allocator.allocate(layout),
        Err(BumpAllocError::OutOfMemory {
            available_size: 0,
            ..
        })
    ));
}
