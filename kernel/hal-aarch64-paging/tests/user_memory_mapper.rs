//! Тесты, проверяющие изоляцию двух независимых `PageMapper`-инстансов,
//! имитирующих per-process user-AS: каждый владеет собственным L0-root,
//! маппинги одного не видны через walk во втором.
#![allow(unsafe_code)]

mod common;

use common::{allocate_buffer, make_mapper};
use hal_aarch64_paging::{
    level::{L3, Level},
    mapper::WalkError,
    mem_flags::{Aarch64MemFlags, Access, Shareability},
};
use memory::{
    physical_address::AlignedPhysicalAddress, virtual_address::PageAlignedVirtualAddress,
};

fn user_rw_flags() -> Aarch64MemFlags {
    Aarch64MemFlags::new()
        .af(true)
        .sh(Shareability::Inner)
        .ap(Access::UserRW)
        .attr_index(0)
        .pxn(true)
        .uxn(true)
}

#[test]
fn aarch64_user_memory_mapper_unique_root() {
    // Два независимых буфера -> две независимые base'ы -> разные физические
    // местоположения L0-root (mock-аллокатор раздаёт страницы внутри
    // конкретного буфера; base == первая страница, == root_pa в трансляции).
    let buf_a = allocate_buffer(8);
    let buf_b = allocate_buffer(8);
    let (_mapper_a, base_a) = make_mapper(buf_a);
    let (_mapper_b, base_b) = make_mapper(buf_b);

    assert_ne!(
        base_a, base_b,
        "two PageMappers must own distinct root pages"
    );
}

#[test]
fn aarch64_user_memory_mapper_isolates_mappings() {
    let buf_a = allocate_buffer(16);
    let buf_b = allocate_buffer(16);
    let (mut mapper_a, _) = make_mapper(buf_a);
    let (mapper_b, _) = make_mapper(buf_b);

    let virt = PageAlignedVirtualAddress::from_usize(0x4000_0000).unwrap();
    let phys =
        AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(0x10_0000).expect("4K aligned PA");
    mapper_a
        .map_page(virt, phys, user_rw_flags())
        .expect("map_page in A");

    // mapper_a знает страницу.
    mapper_a
        .walk_to_l3_leaf(virt)
        .expect("A must walk to leaf after map");

    // mapper_b - нет: walk возвращает NotMapped.
    assert_eq!(mapper_b.walk_to_l3_leaf(virt), Err(WalkError::NotMapped));
}
