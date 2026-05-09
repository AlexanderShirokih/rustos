//! Тесты `mapper::unmap_range` и `mapper::free_all`: снятие 4K/2M/1G leaf'ов,
//! освобождение опустевших промежуточных таблиц, отказ на partial-block,
//! полный обход дерева в Drop-пути.
#![allow(unsafe_code)]

mod common;

use common::{allocate_buffer, make_mapper};
use hal_aarch64_paging::{
    level::{L1, L2, L3, Level},
    mapper::{UnmapError, UnmapVisitor, free_all, unmap_range},
    mem_flags::{Aarch64MemFlags, Access, Shareability},
};
use memory::{
    physical_address::{AlignedPhysicalAddress, PageAlignedAddress},
    virtual_address::PageAlignedVirtualAddress,
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

#[test]
fn unmap_range_releases_4k_leaf_and_intermediate_tables() {
    let buf = allocate_buffer(16);
    let (mut mapper, _) = make_mapper(buf);
    let virt = PageAlignedVirtualAddress::from_usize(0x4000_0000).unwrap();
    let phys = AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(0x10_0000).expect("aligned PA");
    mapper.map_page(virt, phys, user_rw()).expect("map_page");

    let mut visitor = Counter::default();
    // SAFETY: mapper эксклюзивно используется в этом тесте; root и alloc валидны.
    unsafe {
        unmap_range(
            mapper.root_ptr(),
            mapper.alloc(),
            virt.as_usize(),
            1usize << L3::SHIFT,
            &mut visitor,
        )
        .expect("unmap_range");
    }
    assert_eq!(visitor.leaves.len(), 1);
    assert_eq!(visitor.leaves[0].1, L3::SHIFT);
    // Освобождены L1, L2, L3 (3 промежуточные таблицы).
    assert_eq!(visitor.tables.len(), 3);
}

#[test]
fn unmap_range_whole_l2_block() {
    let buf = allocate_buffer(8);
    let (mut mapper, _) = make_mapper(buf);
    let virt =
        memory::virtual_address::AlignedVirtualAddress::<{ L2::SHIFT }>::from_usize(0x4000_0000)
            .unwrap();
    let phys =
        AlignedPhysicalAddress::<{ L2::SHIFT }>::from_usize(0x4000_0000).expect("L2 aligned PA");
    mapper
        .map_page(virt, phys, user_rw())
        .expect("map L2 block");

    let mut visitor = Counter::default();
    // SAFETY: см. выше.
    unsafe {
        unmap_range(
            mapper.root_ptr(),
            mapper.alloc(),
            0x4000_0000,
            1usize << L2::SHIFT,
            &mut visitor,
        )
        .expect("unmap whole 2M block");
    }
    assert_eq!(visitor.leaves.len(), 1);
    assert_eq!(visitor.leaves[0].1, L2::SHIFT);
    // Под L2 block не было L3-таблицы; освобождаются L1, L2.
    assert_eq!(visitor.tables.len(), 2);
}

#[test]
fn unmap_range_partial_inside_block_rejected() {
    let buf = allocate_buffer(8);
    let (mut mapper, _) = make_mapper(buf);
    let virt =
        memory::virtual_address::AlignedVirtualAddress::<{ L2::SHIFT }>::from_usize(0x4000_0000)
            .unwrap();
    let phys =
        AlignedPhysicalAddress::<{ L2::SHIFT }>::from_usize(0x4000_0000).expect("L2 aligned PA");
    mapper
        .map_page(virt, phys, user_rw())
        .expect("map L2 block");

    let mut visitor = Counter::default();
    // 4K внутри 2M-блока (offset 4K от начала).
    let inside = 0x4000_0000 + (1usize << L3::SHIFT);
    // SAFETY: см. выше.
    let err = unsafe {
        unmap_range(
            mapper.root_ptr(),
            mapper.alloc(),
            inside,
            1usize << L3::SHIFT,
            &mut visitor,
        )
        .expect_err("partial unmap inside L2 block must reject")
    };
    assert_eq!(err, UnmapError::UnsupportedPartialBlock);
    assert!(visitor.leaves.is_empty());
    assert!(visitor.tables.is_empty());
}

#[test]
fn unmap_range_unmapped_returns_not_mapped() {
    let buf = allocate_buffer(4);
    let (mapper, _) = make_mapper(buf);

    let mut visitor = Counter::default();
    // SAFETY: см. выше.
    let err = unsafe {
        unmap_range(
            mapper.root_ptr(),
            mapper.alloc(),
            0x4000_0000,
            1usize << L3::SHIFT,
            &mut visitor,
        )
        .expect_err("unmap of unmapped VA must fail")
    };
    assert_eq!(err, UnmapError::NotMapped);
}

#[test]
fn free_all_walks_mixed_leaf_kinds() {
    let buf = allocate_buffer(16);
    let (mut mapper, _) = make_mapper(buf);

    let virt_4k = PageAlignedVirtualAddress::from_usize(0x4000_0000).unwrap();
    let phys_4k = AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(0x10_0000).unwrap();
    mapper
        .map_page(virt_4k, phys_4k, user_rw())
        .expect("4K map");

    let virt_2m =
        memory::virtual_address::AlignedVirtualAddress::<{ L2::SHIFT }>::from_usize(0x8000_0000)
            .unwrap();
    let phys_2m =
        AlignedPhysicalAddress::<{ L2::SHIFT }>::from_usize(0x8000_0000).expect("2M aligned");
    mapper
        .map_page(virt_2m, phys_2m, user_rw())
        .expect("2M map");

    let virt_1g =
        memory::virtual_address::AlignedVirtualAddress::<{ L1::SHIFT }>::from_usize(0xC000_0000)
            .unwrap();
    let phys_1g = AlignedPhysicalAddress::<{ L1::SHIFT }>::from_usize(0xC000_0000).expect("1G");
    mapper
        .map_page(virt_1g, phys_1g, user_rw())
        .expect("1G map");

    let mut visitor = Counter::default();
    // SAFETY: см. выше.
    unsafe {
        free_all(mapper.root_ptr(), mapper.alloc(), false, &mut visitor);
    }
    assert_eq!(visitor.leaves.len(), 3);
    let shifts: Vec<u8> = visitor.leaves.iter().map(|(_, s)| *s).collect();
    assert!(shifts.contains(&L3::SHIFT));
    assert!(shifts.contains(&L2::SHIFT));
    assert!(shifts.contains(&L1::SHIFT));
    // Все 3 leaf'а в одном L0/L1: L3 + 2*L2 + L1 = 4 child-таблицы.
    assert_eq!(visitor.tables.len(), 4);
}

/// Регресс: walker должен индексировать L0 через `(va >> 39) & 0x1FF`,
/// а не через `(va - 0) / (512G)`. Для kernel higher-half VA
/// (`0xFFFF_FF80_...`) деление даёт OOB-индекс и OOB на `get_raw`.
/// `MmioServiceImpl` cleanup и любой kernel-side unmap полагаются на
/// этот путь.
#[test]
fn unmap_range_higher_half_va() {
    let buf = allocate_buffer(16);
    let (mut mapper, _) = make_mapper(buf);
    // Higher-half VA: bit 47 = 1, bits 48..63 sign-extended.
    let kernel_va_raw: usize = 0xFFFF_FF80_4000_0000;
    let virt = PageAlignedVirtualAddress::from_usize(kernel_va_raw).unwrap();
    let phys = AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(0x10_0000).unwrap();
    mapper
        .map_page(virt, phys, user_rw())
        .expect("map kernel-half");

    let mut visitor = Counter::default();
    // SAFETY: mapper эксклюзивен в тесте.
    unsafe {
        unmap_range(
            mapper.root_ptr(),
            mapper.alloc(),
            kernel_va_raw,
            1usize << L3::SHIFT,
            &mut visitor,
        )
        .expect("unmap higher-half");
    }
    assert_eq!(visitor.leaves.len(), 1);
    // Leaf VA должна остаться canonical higher-half, а не свернуться в lower.
    assert_eq!(visitor.leaves[0].0.as_usize(), kernel_va_raw);
    assert_eq!(visitor.leaves[0].1, L3::SHIFT);
    assert_eq!(visitor.tables.len(), 3);
}

/// Регресс на overflow: 1G block-leaf на последнем 1G канонического AS.
/// `entry_va + entry_size` переполнило бы u64, exclusive walker отвалится
/// либо silent-wrap'нет в 0; inclusive (entry_last == usize::MAX) - OK.
#[test]
fn unmap_range_last_1g_block() {
    let buf = allocate_buffer(8);
    let (mut mapper, _) = make_mapper(buf);
    let last_1g_va: usize = 0xFFFF_FFFF_C000_0000;
    let virt =
        memory::virtual_address::AlignedVirtualAddress::<{ L1::SHIFT }>::from_usize(last_1g_va)
            .unwrap();
    let phys = AlignedPhysicalAddress::<{ L1::SHIFT }>::from_usize(0x4000_0000).unwrap();
    mapper
        .map_page(virt, phys, user_rw())
        .expect("map last 1G block");

    let mut visitor = Counter::default();
    // SAFETY: mapper эксклюзивен в тесте.
    unsafe {
        unmap_range(
            mapper.root_ptr(),
            mapper.alloc(),
            last_1g_va,
            1usize << L1::SHIFT,
            &mut visitor,
        )
        .expect("unmap last 1G block");
    }
    assert_eq!(visitor.leaves.len(), 1);
    assert_eq!(visitor.leaves[0].1, L1::SHIFT);
    assert_eq!(visitor.leaves[0].0.as_usize(), last_1g_va);
    assert_eq!(visitor.tables.len(), 1);
}
