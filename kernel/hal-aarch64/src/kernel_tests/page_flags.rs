//! Платформенная проверка трансляции `MemFlags` в биты leaf-PTE user-AS.
//!
//! Единственное место, где наблюдается фактическая кодировка прав страницы:
//! host-тесты `hal-aarch64-paging` работают с `Aarch64MemFlags` уже после
//! конвертации, а из EL0 биты PTE не видны вовсе. Проверяются оба направления
//! `map_exact` -> `remap`: смена флагов идёт отдельным путём
//! (`update_l3_flags`).
#![allow(unsafe_code)]

extern crate alloc;

use hal_aarch64_paging::memory_attribute::MemoryAttribute;
use kernel_tests::kernel_test;
use kernelspace::syscall_bridge;
use memory::{
    MemFlags,
    mem_flags::{Owners, UncachedPermission},
    memory_mapper::MemoryMapper,
    virtual_address::PageAlignedVirtualAddress,
};
use scheduler::AddressSpace;

use super::probe_va;
use crate::memory::address_space_factory::UserAarch64MemoryMapper;

const PAGE_SIZE: usize = memory::PAGE_SIZE.get();

const PROBE_VA: usize = probe_va::PAGE_FLAGS.base();

struct Leaf {
    raw: u64,
}

impl Leaf {
    fn is_page(&self) -> bool {
        self.raw & 0b11 == 0b11
    }
    fn ap(&self) -> u64 {
        (self.raw >> 6) & 0b11
    }
    fn af(&self) -> u64 {
        (self.raw >> 10) & 1
    }
    fn ng(&self) -> u64 {
        (self.raw >> 11) & 1
    }
    fn pxn(&self) -> u64 {
        (self.raw >> 53) & 1
    }
    fn uxn(&self) -> u64 {
        (self.raw >> 54) & 1
    }
    fn attr_index(&self) -> u8 {
        ((self.raw >> 2) & 0b111) as u8
    }
}

fn downcast_user_mapper(mapper: &(dyn MemoryMapper + Send + Sync)) -> &UserAarch64MemoryMapper {
    mapper
        .as_any()
        .downcast_ref::<UserAarch64MemoryMapper>()
        .expect("user-AS mapper must be Aarch64MemoryMapper")
}

fn leaf(mapper: &(dyn MemoryMapper + Send + Sync), va: PageAlignedVirtualAddress) -> Leaf {
    Leaf {
        raw: downcast_user_mapper(mapper)
            .query_leaf_raw(va)
            .expect("leaf must exist"),
    }
}

#[kernel_test]
fn user_page_flags_map_and_remap() {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    let fa = syscall_bridge::frame_allocator().expect("FrameAllocator must be installed");

    let user_as = AddressSpace::new_user(factory).expect("create user AS");
    let mapper = user_as.mapper().expect("user variant has mapper");
    let frame = fa.allocate_frame().expect("RAM frame available");
    let va = PageAlignedVirtualAddress::from_usize(PROBE_VA).expect("PROBE_VA aligned");

    mapper
        .map_exact(va, frame.page_address(), PAGE_SIZE, MemFlags::user_rw())
        .expect("map_exact probe page as UserRW");

    let rw = leaf(mapper, va);
    kernel_tests::kassert!(rw.is_page());
    kernel_tests::kassert_eq!(rw.ap(), 0b01);
    kernel_tests::kassert_eq!(rw.af(), 1);
    kernel_tests::kassert_eq!(rw.ng(), 1);
    kernel_tests::kassert_eq!(rw.pxn(), 1);
    kernel_tests::kassert_eq!(rw.uxn(), 1);
    kernel_tests::kassert_eq!(rw.attr_index(), MemoryAttribute::Cached.index());

    mapper
        .remap(va, PAGE_SIZE, MemFlags::user_rx())
        .expect("remap probe page to UserRX");

    let rx = leaf(mapper, va);
    kernel_tests::kassert!(rx.is_page());
    kernel_tests::kassert_eq!(rx.ap(), 0b11);
    kernel_tests::kassert_eq!(rx.af(), 1);
    kernel_tests::kassert_eq!(rx.ng(), 1);
    kernel_tests::kassert_eq!(rx.pxn(), 1);
    kernel_tests::kassert_eq!(rx.uxn(), 0);
    kernel_tests::kassert_eq!(rx.attr_index(), MemoryAttribute::Cached.index());

    // AS ни разу не активировался, его entries в TLB нет - drop безопасен.
    // Фрейм страницы пришёл от вызывающего, drop leaf-дескрипторов в FA его
    // не возвращает: сами отдаём.
    drop(user_as);
    let _ = fa.deallocate_frame(frame);
}

// Regression guard: индекс слота в PTE и состав MAIR_EL1 определены в разных
// крейтах, и разошедшись, переводят страницу в чужой тип памяти без отказа.
#[kernel_test]
fn user_page_profiles_select_their_mair_slots() {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    let fa = syscall_bridge::frame_allocator().expect("FrameAllocator must be installed");

    let user_as = AddressSpace::new_user(factory).expect("create user AS");
    let mapper = user_as.mapper().expect("user variant has mapper");
    let frame = fa.allocate_frame().expect("RAM frame available");
    let va = PageAlignedVirtualAddress::from_usize(PROBE_VA).expect("PROBE_VA aligned");

    let owners = Owners::kernel(UncachedPermission::writable());
    let cases = [
        (MemFlags::user_rw(), MemoryAttribute::Cached),
        (
            MemFlags::WriteCombining(owners),
            MemoryAttribute::WriteCombining,
        ),
        (MemFlags::Device(owners), MemoryAttribute::Device),
    ];

    mapper
        .map_exact(va, frame.page_address(), PAGE_SIZE, cases[0].0)
        .expect("map_exact probe page");

    for (flags, attribute) in cases {
        mapper
            .remap(va, PAGE_SIZE, flags)
            .expect("remap probe page to the next profile");
        let observed = leaf(mapper, va);
        kernel_tests::kassert!(observed.is_page());
        kernel_tests::kassert_eq!(observed.attr_index(), attribute.index());

        let slot = (mair_el1() >> (u64::from(attribute.index()) * 8)) & 0xFF;
        kernel_tests::kassert_eq!(slot as u8, attribute.encoding());
    }

    drop(user_as);
    let _ = fa.deallocate_frame(frame);
}

fn mair_el1() -> u64 {
    // SAFETY: чтение MAIR_EL1 допустимо на EL1 и не меняет состояние.
    unsafe { crate::read_sysreg!(mair_el1) }
}
