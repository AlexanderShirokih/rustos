//! Error-пути `map_page` (`AlreadyMapped`, `NeedsSmallerPages`, `OutOfMemory`),
//! многостраничный `unmap_range` (>1 leaf, пересечение границ L2/L1),
//! невыровненные адреса и `free_all(high_half=true)`.
#![allow(unsafe_code)]

mod common;

use common::{allocate_buffer, make_mapper};
use hal_aarch64_paging::{
    level::{L1, L2, L3, Level},
    mapper::{MapError, UnmapVisitor, free_all, unmap_range},
    mem_flags::{Aarch64MemFlags, Access, Shareability},
};
use memory::{
    physical_address::{AlignedPhysicalAddress, PageAlignedAddress},
    virtual_address::{AlignedVirtualAddress, PageAlignedVirtualAddress},
};

fn user_rw() -> Aarch64MemFlags {
    Aarch64MemFlags::new()
        .af(true)
        .sh(Shareability::Inner)
        .ap(Access::UserRW)
        .attr_index(0)
        .pxn(true)
        .uxn(true)
}

#[derive(Default)]
struct Counter {
    leaves: Vec<(PageAlignedVirtualAddress, u8)>,
    tables: Vec<PageAlignedAddress>,
}

impl UnmapVisitor for Counter {
    fn on_leaf(&mut self, va: PageAlignedVirtualAddress, leaf_shift: u8, _raw: u64) {
        self.leaves.push((va, leaf_shift));
    }
    fn on_free_table(&mut self, pa: PageAlignedAddress) {
        self.tables.push(pa);
    }
}

// Error-пути map_page

#[test]
fn map_page_already_mapped_4k() {
    let buf = allocate_buffer(16);
    let (mut mapper, _) = make_mapper(buf);
    let virt = PageAlignedVirtualAddress::from_usize(0x4000_0000).unwrap();
    let phys = AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(0x10_0000).unwrap();

    mapper.map_page(virt, phys, user_rw()).expect("первый map");
    // Повторный map той же страницы -> AlreadyMapped.
    let err = mapper
        .map_page(virt, phys, user_rw())
        .expect_err("повторный map должен отказать");
    assert!(matches!(err, MapError::AlreadyMapped), "получено {err:?}");
}

#[test]
fn map_page_already_mapped_2m_block() {
    let buf = allocate_buffer(16);
    let (mut mapper, _) = make_mapper(buf);
    let virt = AlignedVirtualAddress::<{ L2::SHIFT }>::from_usize(0x4000_0000).unwrap();
    let phys = AlignedPhysicalAddress::<{ L2::SHIFT }>::from_usize(0x4000_0000).unwrap();

    mapper.map_page(virt, phys, user_rw()).expect("map 2M");
    let err = mapper
        .map_page(virt, phys, user_rw())
        .expect_err("повторный 2M map должен отказать");
    assert!(matches!(err, MapError::AlreadyMapped), "получено {err:?}");
}

#[test]
fn map_page_needs_smaller_pages_when_table_present() {
    // Сначала маппим 4K-страницу - это создаёт L3-таблицу под L2-ячейкой.
    // Затем попытка замапить 2M-блок по тому же L2-индексу должна вернуть
    // NeedsSmallerPages (там уже Table, не Invalid).
    let buf = allocate_buffer(16);
    let (mut mapper, _) = make_mapper(buf);

    let page_virt = PageAlignedVirtualAddress::from_usize(0x4000_0000).unwrap();
    let page_phys = AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(0x10_0000).unwrap();
    mapper
        .map_page(page_virt, page_phys, user_rw())
        .expect("4K map");

    // 2M-блок, накрывающий тот же L2-вход (0x4000_0000 выровнен на 2M).
    let block_virt = AlignedVirtualAddress::<{ L2::SHIFT }>::from_usize(0x4000_0000).unwrap();
    let block_phys = AlignedPhysicalAddress::<{ L2::SHIFT }>::from_usize(0x4000_0000).unwrap();
    let err = mapper
        .map_page(block_virt, block_phys, user_rw())
        .expect_err("2M поверх существующей L3-таблицы");
    assert!(
        matches!(err, MapError::NeedsSmallerPages),
        "получено {err:?}"
    );
}

#[test]
fn map_page_out_of_memory_on_alloc_exhaustion() {
    // Буфер всего на 1 страницу: после выделения root-таблицы у MockAlloc
    // не осталось страниц под L1/L2/L3 -> ensure_next вернёт OutOfMemory.
    let buf = allocate_buffer(1);
    let (mut mapper, _) = make_mapper(buf);
    let virt = PageAlignedVirtualAddress::from_usize(0x4000_0000).unwrap();
    let phys = AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(0x10_0000).unwrap();

    let err = mapper
        .map_page(virt, phys, user_rw())
        .expect_err("исчерпание аллокатора");
    assert!(matches!(err, MapError::OutOfMemory), "получено {err:?}");
}

// Невыровненные адреса (type-level guard)

#[test]
fn unaligned_va_cannot_be_constructed() {
    // map_page принимает только AlignedVirtualAddress; невыровненный адрес
    // отбрасывается на этапе конструирования, а не паникой внутри маппера.
    assert!(PageAlignedVirtualAddress::from_usize(0x4000_0000 + 1).is_none());
    assert!(AlignedVirtualAddress::<{ L2::SHIFT }>::from_usize(0x4000_0000 + 0x1000).is_none());
    assert!(AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(0x10_0001).is_none());
}

// Многостраничный unmap_range

#[test]
fn unmap_range_multiple_4k_leaves() {
    let buf = allocate_buffer(16);
    let (mut mapper, _) = make_mapper(buf);

    // Три смежные 4K-страницы.
    let base = 0x4000_0000usize;
    for i in 0..3 {
        let va = PageAlignedVirtualAddress::from_usize(base + i * (1 << L3::SHIFT)).unwrap();
        let pa =
            AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(0x10_0000 + i * (1 << L3::SHIFT))
                .unwrap();
        mapper.map_page(va, pa, user_rw()).expect("map 4K");
    }

    let mut visitor = Counter::default();
    // SAFETY: mapper эксклюзивен в тесте.
    unsafe {
        unmap_range(
            mapper.root_ptr(),
            mapper.alloc(),
            base,
            3 * (1usize << L3::SHIFT),
            &mut visitor,
        )
        .expect("unmap 3 страниц");
    }
    assert_eq!(visitor.leaves.len(), 3, "должны сняться 3 leaf'а");
    for leaf in &visitor.leaves {
        assert_eq!(leaf.1, L3::SHIFT);
    }
}

#[test]
fn unmap_range_crossing_l2_boundary() {
    // Две 2M-блок-leaf'а в соседних L2-ячейках одного L1: диапазон 4M пересекает
    // границу L2.
    let buf = allocate_buffer(16);
    let (mut mapper, _) = make_mapper(buf);

    let two_m = 1usize << L2::SHIFT;
    let base = 0x4000_0000usize;
    for i in 0..2 {
        let va = AlignedVirtualAddress::<{ L2::SHIFT }>::from_usize(base + i * two_m).unwrap();
        let pa = AlignedPhysicalAddress::<{ L2::SHIFT }>::from_usize(base + i * two_m).unwrap();
        mapper.map_page(va, pa, user_rw()).expect("map 2M");
    }

    let mut visitor = Counter::default();
    // SAFETY: mapper эксклюзивен в тесте.
    unsafe {
        unmap_range(
            mapper.root_ptr(),
            mapper.alloc(),
            base,
            2 * two_m,
            &mut visitor,
        )
        .expect("unmap 2x2M");
    }
    assert_eq!(visitor.leaves.len(), 2);
    for leaf in &visitor.leaves {
        assert_eq!(leaf.1, L2::SHIFT);
    }
}

// free_all(high_half = true)

#[test]
fn free_all_high_half_walks_kernel_space() {
    let buf = allocate_buffer(16);
    let (mut mapper, _) = make_mapper(buf);

    // Higher-half (kernel) VA.
    let kernel_va: usize = 0xFFFF_FF80_4000_0000;
    let virt = PageAlignedVirtualAddress::from_usize(kernel_va).unwrap();
    let phys = AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(0x10_0000).unwrap();
    mapper
        .map_page(virt, phys, user_rw())
        .expect("map kernel-half");

    let mut visitor = Counter::default();
    // SAFETY: mapper эксклюзивен в тесте.
    unsafe {
        free_all(mapper.root_ptr(), mapper.alloc(), true, &mut visitor);
    }
    // Leaf в higher-half должен быть обойдён, VA остаётся canonical.
    assert_eq!(visitor.leaves.len(), 1);
    assert_eq!(visitor.leaves[0].0.as_usize(), kernel_va);
    assert_eq!(visitor.leaves[0].1, L3::SHIFT);
    // L1+L2+L3 child-таблицы освобождены.
    assert_eq!(visitor.tables.len(), 3);
    let _ = L1::SHIFT;
}
