mod common;

use collections::Vec;
use common::{make_va_range, va};
use memory::region_manager::RegionManager;

// =============================================================================
// 1. Базовые случаи
// =============================================================================

#[test]
fn empty_manager_returns_none() {
    let regions = Vec::new();
    let manager = RegionManager::new(regions);

    let result = manager.next_free(va(0), 0x100);
    assert!(result.is_none(), "пустой менеджер должен возвращать None");
}

#[test]
fn next_free_from_start() {
    let mut regions = Vec::new();
    regions.push(make_va_range(0x1000, 0x2000)).unwrap();

    let manager = RegionManager::new(regions);

    let result = manager.next_free(va(0x1000), 0x100);
    assert_eq!(result, Some(va(0x1000)));
}

#[test]
fn next_free_from_middle() {
    let mut regions = Vec::new();
    regions.push(make_va_range(0x1000, 0x2000)).unwrap();

    let manager = RegionManager::new(regions);

    // cursor внутри региона
    let result = manager.next_free(va(0x1500), 0x100);
    assert_eq!(result, Some(va(0x1500)));
}

// =============================================================================
// 2. Граничные позиции cursor
// =============================================================================

#[test]
fn next_free_cursor_before_region() {
    let mut regions = Vec::new();
    regions.push(make_va_range(0x1000, 0x2000)).unwrap();

    let manager = RegionManager::new(regions);

    // cursor до начала региона - должен вернуть начало региона
    let result = manager.next_free(va(0x500), 0x100);
    assert_eq!(result, Some(va(0x1000)));
}

#[test]
fn next_free_cursor_after_all_regions() {
    let mut regions = Vec::new();
    regions.push(make_va_range(0x1000, 0x2000)).unwrap();

    let manager = RegionManager::new(regions);

    // cursor после конца всех регионов
    let result = manager.next_free(va(0x3000), 0x100);
    assert!(result.is_none(), "cursor после всех регионов должен возвращать None");
}

// =============================================================================
// 3. Размер и несколько регионов
// =============================================================================

#[test]
fn next_free_size_exceeds_region() {
    let mut regions = Vec::new();
    // Маленький регион (размер 0x100)
    regions.push(make_va_range(0x1000, 0x10FF)).unwrap();
    // Большой регион (размер 0x1000)
    regions.push(make_va_range(0x2000, 0x3000)).unwrap();

    let manager = RegionManager::new(regions);

    // Запрашиваем размер больше первого региона - должен найти во втором
    let result = manager.next_free(va(0), 0x200);
    assert_eq!(result, Some(va(0x2000)));
}

#[test]
fn next_free_skips_to_next_region() {
    let mut regions = Vec::new();
    regions.push(make_va_range(0x1000, 0x2000)).unwrap();
    regions.push(make_va_range(0x3000, 0x4000)).unwrap();

    let manager = RegionManager::new(regions);

    // cursor после первого региона, но до второго
    let result = manager.next_free(va(0x2500), 0x100);
    assert_eq!(result, Some(va(0x3000)));
}
