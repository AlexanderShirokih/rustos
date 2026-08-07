//! Проводка ASID до железа: аллокатор ASID покрыт host-тестами
//! `hal-aarch64-asid`, здесь проверяется, что выданное им значение доезжает до
//! `TTBR0_EL1[63:48]`, что `TCR_EL1.AS` согласован с обнаруженной шириной ASID
//! и что цепочка переключений не портит трансляции (rollover).
//!
//! Маркировка user-страниц `nG=1` проверяется в [`super::page_flags`].
#![allow(unsafe_code)]

extern crate alloc;

use alloc::sync::Arc;

use kernel_tests::kernel_test;
use kernelspace::syscall_bridge;
use memory::{
    MemFlags,
    mem_flags::{AccessMode, Executable, Owners, PrivateMemoryPermission},
    physical_address::PageAlignedAddress,
    virtual_address::PageAlignedVirtualAddress,
};

const PAGE_SIZE: usize = memory::PAGE_SIZE.get();
use scheduler::{AddressSpace, ArchContext};

use super::probe_va;
use crate::{
    consts::HIGHER_HALF_BASE,
    memory::regs::{common::EL1, id_aa64mmfr0::IdAa64Mmfr0},
    sched::Aarch64Context,
};

const PROBE_VA: usize = probe_va::ASID.base();

fn user_rw_flags() -> MemFlags {
    MemFlags::Private(Owners {
        kernel: PrivateMemoryPermission {
            access: AccessMode::Writable,
            executable: Executable::NotAllowed,
        },
        user: PrivateMemoryPermission {
            access: AccessMode::Writable,
            executable: Executable::NotAllowed,
        },
    })
}

fn allocate_probe_page() -> (*mut u8, PageAlignedAddress) {
    let fa = syscall_bridge::frame_allocator().expect("FrameAllocator must be installed");
    // Фрейм течёт намеренно: после маппинга в user-AS вернуть его нельзя.
    let frame = fa.allocate_frame().expect("RAM frame available");
    let pa = frame.page_address();
    let ptr = (HIGHER_HALF_BASE + pa.as_usize()) as *mut u8;
    // SAFETY: linear higher-half-маппинг покрывает все RAM-фреймы;
    // фрейм только что выдан и эксклюзивно наш.
    unsafe {
        core::ptr::write_bytes(ptr, 0, PAGE_SIZE);
    }
    (ptr, pa)
}

fn read_ttbr0() -> u64 {
    let raw: u64;
    // SAFETY: чтение TTBR0_EL1 на EL1 разрешено и безопасно.
    unsafe {
        core::arch::asm!(
            "mrs {raw}, ttbr0_el1",
            raw = out(reg) raw,
            options(nostack, nomem, preserves_flags),
        );
    }
    raw
}

fn read_tcr() -> u64 {
    let raw: u64;
    // SAFETY: чтение TCR_EL1 на EL1 разрешено и безопасно.
    unsafe {
        core::arch::asm!(
            "mrs {raw}, tcr_el1",
            raw = out(reg) raw,
            options(nostack, nomem, preserves_flags),
        );
    }
    raw
}

fn asid_of(ttbr0: u64) -> u64 {
    (ttbr0 >> 48) & 0xFFFF
}

fn make_user_as_with_probe_page(va: usize, sentinel: u64) -> (Arc<AddressSpace>, *mut u8) {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    let user_as = AddressSpace::new_user(factory).expect("create user AS");
    let mapper = user_as.mapper().expect("user variant has mapper");

    let (kheap, pa) = allocate_probe_page();
    let aligned_va = PageAlignedVirtualAddress::from_usize(va).expect("4K-aligned VA");
    mapper
        .map_exact(aligned_va, pa, PAGE_SIZE, user_rw_flags())
        .expect("map_exact probe page");

    // SAFETY: kheap - свежевыделенная страница, эксклюзивно владеемая нами.
    #[allow(clippy::cast_ptr_alignment)]
    unsafe {
        kheap.cast::<u64>().write_volatile(sentinel);
    }

    (user_as, kheap)
}

/// Каждый user-AS получает ненулевой ASID, разные AS - разные ASID, и один и
/// тот же AS сохраняет свой между переключениями.
#[kernel_test]
fn ttbr0_carries_stable_distinct_asids() {
    let (as_a, _) = make_user_as_with_probe_page(PROBE_VA, 0x1);
    let (as_b, _) = make_user_as_with_probe_page(PROBE_VA, 0x2);

    Aarch64Context::switch_address_space(as_a.handle());
    let ttbr_a = read_ttbr0();
    Aarch64Context::switch_address_space(as_b.handle());
    let ttbr_b = read_ttbr0();
    // Возврат к A: ASID обязан совпасть с первым наблюдением.
    Aarch64Context::switch_address_space(as_a.handle());
    let ttbr_a_again = read_ttbr0();
    Aarch64Context::switch_address_space(None);

    kernel_tests::kassert!(asid_of(ttbr_a) != 0);
    kernel_tests::kassert!(asid_of(ttbr_b) != 0);
    kernel_tests::kassert!(asid_of(ttbr_a) != asid_of(ttbr_b));
    kernel_tests::kassert_eq!(ttbr_a, ttbr_a_again);

    drop(as_a);
    drop(as_b);
}

/// `TCR_EL1.AS` выставлен по ширине ASID, обнаруженной в `ID_AA64MMFR0_EL1`.
#[kernel_test]
fn tcr_as_matches_runtime_asid_width() {
    let width = IdAa64Mmfr0::<EL1>::asid_width();
    let tcr = read_tcr();
    let as_bit = (tcr >> 36) & 1;
    let expected = match width {
        crate::memory::regs::id_aa64mmfr0::AsidWidth::Bits16 => 1,
        crate::memory::regs::id_aa64mmfr0::AsidWidth::Bits8 => 0,
    };
    kernel_tests::kassert_eq!(as_bit, expected);
}

/// Активирует много AS подряд, чтобы спровоцировать ASID rollover.
#[kernel_test]
fn rollover_smoke() {
    let (probe_as, kva_a) = make_user_as_with_probe_page(PROBE_VA, 0x1234);
    let h_probe = probe_as.handle().expect("handle");
    Aarch64Context::switch_address_space(Some(h_probe));
    // SAFETY: PROBE_VA замаплен с UserRW, чтение валидно.
    let observed = unsafe { (PROBE_VA as *const u64).read_volatile() };
    kernel_tests::kassert_eq!(observed, 0x1234);
    Aarch64Context::switch_address_space(None);

    let (shared_kheap, shared_pa) = allocate_probe_page();
    // SAFETY: shared_kheap - свежевыделенная страница, эксклюзивно владеемая.
    #[allow(clippy::cast_ptr_alignment)]
    unsafe {
        shared_kheap.cast::<u64>().write_volatile(0x5555);
    }

    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    let probe_va = PageAlignedVirtualAddress::from_usize(PROBE_VA + 0x1000).expect("aligned");

    // >255 итераций ради 8-bit-режима. В 16-bit rollover не сработает, но
    // цепочка переключений всё равно проверяется.
    for _ in 0..260 {
        let a = AddressSpace::new_user(factory).expect("create AS");
        let mapper = a.mapper().expect("user mapper");
        mapper
            .map_exact(probe_va, shared_pa, PAGE_SIZE, user_rw_flags())
            .expect("map_exact");
        let h = a.handle().expect("handle");
        Aarch64Context::switch_address_space(Some(h));
        // SAFETY: PROBE_VA+4K замаплен с UserRW.
        let v = unsafe { ((PROBE_VA + 0x1000) as *const u64).read_volatile() };
        kernel_tests::kassert_eq!(v, 0x5555);
        Aarch64Context::switch_address_space(None);
        drop(a);
    }

    // ещё видна (т.е. trace переключений ASID не повредил трансляции).
    let h_probe2 = probe_as.handle().expect("handle 2");
    Aarch64Context::switch_address_space(Some(h_probe2));
    // SAFETY: см. выше.
    let observed2 = unsafe { (PROBE_VA as *const u64).read_volatile() };
    kernel_tests::kassert_eq!(observed2, 0x1234);
    Aarch64Context::switch_address_space(None);

    let _ = kva_a;
    let _ = shared_kheap;
    drop(probe_as);
}
