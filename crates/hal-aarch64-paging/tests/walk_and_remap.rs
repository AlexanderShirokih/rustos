#![allow(unsafe_code)]
#![allow(clippy::similar_names)]

mod common;

use common::{allocate_buffer, make_mapper};
use hal_aarch64_paging::{
    entry::{Entry, Page},
    level::{L2, L3, Level, PagePa},
    mapper::{self, WalkError},
    mem_flags::{Aarch64MemFlags, Access, Shareability},
};
use memory::{
    aligned::Aligned,
    physical_address::{AlignedPhysicalAddress, PageAlignedAddress},
    virtual_address::PageAlignedVirtualAddress,
};

const PAGE_SIZE: usize = PageAlignedAddress::ALIGNMENT;

fn user_rw_flags() -> Aarch64MemFlags {
    Aarch64MemFlags::new()
        .af(true)
        .sh(Shareability::Inner)
        .ap(Access::UserRW)
        .attr_index(0)
        .pxn(true)
        .uxn(true)
}

fn user_ro_flags() -> Aarch64MemFlags {
    Aarch64MemFlags::new()
        .af(true)
        .sh(Shareability::Inner)
        .ap(Access::UserRO)
        .attr_index(0)
        .pxn(true)
        .uxn(true)
}

fn make_pa(byte_offset: usize) -> PagePa {
    AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(byte_offset)
        .expect("offset must be page aligned")
}

#[test]
fn walk_returns_ok_for_mapped_4k_page() {
    let buffer = allocate_buffer(16);
    let (mut mapper, _base) = make_mapper(buffer);

    let virt = PageAlignedVirtualAddress::from_usize(0x4000_0000).unwrap();
    let phys = make_pa(0x10_0000);
    mapper.map_page(virt, phys, user_rw_flags()).unwrap();

    let (l3, idx) = mapper.walk_to_l3_leaf(virt).expect("walk should succeed");
    assert!(!l3.is_null());
    // PageAlignedVirtualAddress::index<L3> = (va >> 12) & 0x1FF
    let expected_idx = (0x4000_0000usize >> 12) & 0x1FF;
    assert_eq!(idx, expected_idx);

    // SAFETY: walk вернул валидный (l3, idx).
    let raw = unsafe { (*l3).get_raw(idx) };
    assert_eq!(raw & 0b11, 0b11, "leaf should be Page descriptor");
}

#[test]
fn walk_returns_not_mapped_for_empty_root() {
    let buffer = allocate_buffer(4);
    let (mapper, _base) = make_mapper(buffer);

    let virt = PageAlignedVirtualAddress::from_usize(0x8000_0000).unwrap();
    assert_eq!(mapper.walk_to_l3_leaf(virt), Err(WalkError::NotMapped));
}

#[test]
fn walk_returns_not_mapped_for_partial_path() {
    let buffer = allocate_buffer(16);
    let (mut mapper, _base) = make_mapper(buffer);

    // Маппим один адрес, чтобы создать частичные таблицы.
    let mapped = PageAlignedVirtualAddress::from_usize(0x4000_0000).unwrap();
    mapper
        .map_page(mapped, make_pa(0x20_0000), user_rw_flags())
        .unwrap();

    // А walk-им в другом L3-индексе той же таблицы - там Invalid.
    let probe = PageAlignedVirtualAddress::from_usize(0x4000_0000 + PAGE_SIZE).unwrap();
    assert_eq!(mapper.walk_to_l3_leaf(probe), Err(WalkError::NotMapped));
}

#[test]
fn walk_returns_hit_block_for_l2_block() {
    let buffer = allocate_buffer(16);
    let (mut mapper, _base) = make_mapper(buffer);

    // Замапим 2M-block через map_page с SHIFT=L2.
    let l2_va =
        memory::virtual_address::AlignedVirtualAddress::<{ L2::SHIFT }>::from_usize(0x2000_0000)
            .unwrap();
    let l2_pa = AlignedPhysicalAddress::<{ L2::SHIFT }>::from_usize(0x4000_0000).expect("aligned");
    mapper.map_page(l2_va, l2_pa, user_rw_flags()).unwrap();

    // 4K-walk внутри блока должен вернуть HitBlock.
    let probe = PageAlignedVirtualAddress::from_usize(0x2000_0000 + 4 * PAGE_SIZE).unwrap();
    assert_eq!(mapper.walk_to_l3_leaf(probe), Err(WalkError::HitBlock));
}

#[test]
fn update_l3_flags_preserves_pa_and_changes_ap() {
    let buffer = allocate_buffer(16);
    let (mut mapper, _base) = make_mapper(buffer);

    let virt = PageAlignedVirtualAddress::from_usize(0x4000_0000).unwrap();
    let phys = make_pa(0x30_0000);
    mapper.map_page(virt, phys, user_ro_flags()).unwrap();

    let (l3, idx) = mapper.walk_to_l3_leaf(virt).unwrap();
    // SAFETY: walk вернул валидный (l3, idx).
    let raw_before = unsafe { (*l3).get_raw(idx) };
    assert_eq!(raw_before & 0b11, 0b11);
    // AP-биты [7:6]; UserRO = 0b11.
    assert_eq!((raw_before >> 6) & 0b11, Access::UserRO as u64);

    // SAFETY: walk вернул валидный (l3, idx).
    unsafe { mapper::update_l3_flags(l3, idx, user_rw_flags()) }.unwrap();

    // SAFETY: walk вернул валидный (l3, idx).
    let raw_after = unsafe { (*l3).get_raw(idx) };
    // PA сохранился.
    let pa_mask = 0x0000_FFFF_FFFF_F000u64;
    assert_eq!(raw_after & pa_mask, raw_before & pa_mask);
    // Desc-type сохранился.
    assert_eq!(raw_after & 0b11, 0b11);
    // AP изменился на UserRW.
    assert_eq!((raw_after >> 6) & 0b11, Access::UserRW as u64);
}

#[test]
fn update_l3_flags_returns_not_mapped_for_invalid_entry() {
    let buffer = allocate_buffer(16);
    let (mut mapper, _base) = make_mapper(buffer);

    // Создадим валидную L3-таблицу через маппинг одной страницы.
    let mapped = PageAlignedVirtualAddress::from_usize(0x4000_0000).unwrap();
    mapper
        .map_page(mapped, make_pa(0x40_0000), user_ro_flags())
        .unwrap();

    // Найдём L3-таблицу через walk и попробуем обновить флаги для соседней
    // (Invalid) записи.
    let (l3, idx) = mapper.walk_to_l3_leaf(mapped).unwrap();
    let invalid_idx = (idx + 1) % 512;
    // SAFETY: l3 валиден, invalid_idx < 512.
    let result = unsafe { mapper::update_l3_flags(l3, invalid_idx, user_rw_flags()) };
    assert_eq!(result, Err(WalkError::NotMapped));
}

#[test]
fn entry_construction_and_update_round_trip() {
    // Проверим, что update_l3_flags оставляет такое же raw, как создание Entry заново.
    let buffer = allocate_buffer(8);
    let (mut mapper, _base) = make_mapper(buffer);

    let virt = PageAlignedVirtualAddress::from_usize(0x6000_0000).unwrap();
    let phys = make_pa(0x50_0000);
    mapper.map_page(virt, phys, user_ro_flags()).unwrap();

    let (l3, idx) = mapper.walk_to_l3_leaf(virt).unwrap();
    // SAFETY: walk вернул валидный (l3, idx).
    unsafe { mapper::update_l3_flags(l3, idx, user_rw_flags()) }.unwrap();

    // SAFETY: walk вернул валидный (l3, idx).
    let raw = unsafe { (*l3).get_raw(idx) };
    let expected = Entry::<L3, Page>::new(phys, user_rw_flags()).raw();
    // raw может отличаться от expected: update_l3_flags не трогает биты,
    // которые не входят в "flags-маску" (например, software-биты ОСей или биты
    // contiguous). Но для базовых пресетов - должны совпасть.
    assert_eq!(raw, expected);
}
