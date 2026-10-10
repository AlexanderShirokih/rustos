//! Модуль проверяет проводку ASID до железа. Аллокатор ASID покрыт host-тестами
//! `hal-aarch64-asid`, а здесь проверяется, что выданное им значение доезжает до
//! `TTBR0_EL1[63:48]`, что `TCR_EL1.AS` согласован с обнаруженной шириной ASID
//! и что ASID, переданный rollover новому адресному пространству, не транслирует через записи
//! TLB прежнего владельца.
//!
//! Маркировка user-страниц `nG=1` проверяется в [`super::page_flags`].
#![allow(unsafe_code)]

extern crate alloc;

use alloc::sync::Arc;

use hal_aarch64_asid::{unpack_asid, unpack_generation};
use kbase::{CriticalSection as _, DaifCriticalSection};
use kernelspace::syscall_bridge;
use memory::{
    MemFlags,
    mem_flags::{AccessMode, CachedPermission, Executable, Owners},
    memory_mapper::{AddressSpaceFactory, AddressSpaceHandle},
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
const PREVIOUS_OWNER_VALUE: u64 = 0xA5A5;
const NEW_OWNER_VALUE: u64 = 0x5A5A;

fn user_rw_flags() -> MemFlags {
    MemFlags::Cached(Owners {
        kernel: CachedPermission {
            access: AccessMode::Writable,
            executable: Executable::NotAllowed,
        },
        user: CachedPermission {
            access: AccessMode::Writable,
            executable: Executable::NotAllowed,
        },
    })
}

fn allocate_probe_page() -> (*mut u64, PageAlignedAddress) {
    let fa = syscall_bridge::frame_allocator().expect("FrameAllocator must be installed");
    // Фрейм намеренно не освобождается: после маппинга в user-AS вернуть его нельзя.
    let frame = fa.allocate_frame().expect("RAM frame available");
    let pa = frame.page_address();
    let ptr = (HIGHER_HALF_BASE + pa.as_usize()) as *mut u64;
    // SAFETY: linear higher-half-маппинг покрывает все RAM-фреймы;
    // фрейм только что выдан и эксклюзивно наш.
    unsafe {
        core::ptr::write_bytes(ptr, 0, PAGE_SIZE / size_of::<u64>());
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

fn make_user_as_with_probe_page(va: usize, sentinel: u64) -> (Arc<AddressSpace>, *mut u64) {
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
    unsafe {
        kheap.write_volatile(sentinel);
    }

    (user_as, kheap)
}

/// Каждый user-AS получает ненулевой ASID, разные AS - разные ASID, и один и
/// тот же AS сохраняет свой между переключениями.
#[test_harness::test]
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

    test_harness::check!(asid_of(ttbr_a) != 0);
    test_harness::check!(asid_of(ttbr_b) != 0);
    test_harness::check!(asid_of(ttbr_a) != asid_of(ttbr_b));
    test_harness::check_eq!(ttbr_a, ttbr_a_again);

    drop(as_a);
    drop(as_b);
}

/// `TCR_EL1.AS` выставлен по ширине ASID, обнаруженной в `ID_AA64MMFR0_EL1`.
#[test_harness::test]
fn tcr_as_matches_runtime_asid_width() {
    let width = IdAa64Mmfr0::<EL1>::asid_width();
    let tcr = read_tcr();
    let as_bit = (tcr >> 36) & 1;
    let expected = match width {
        crate::memory::regs::id_aa64mmfr0::AsidWidth::Bits16 => 1,
        crate::memory::regs::id_aa64mmfr0::AsidWidth::Bits8 => 0,
    };
    test_harness::check_eq!(as_bit, expected);
}

/// ASID, освобождённый rollover, не переносит в новое адресное пространство трансляции прежнего
/// владельца. QEMU сбрасывает TLB при каждой смене ASID в TTBR0, и пропуск сброса виден на железе.
#[test_harness::test]
fn reissued_asid_does_not_translate_through_the_previous_owner() {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    let (previous_owner, _) = make_user_as_with_probe_page(PROBE_VA, PREVIOUS_OWNER_VALUE);
    let (new_owner_page, new_owner_pa) = allocate_probe_page();
    // SAFETY: страница только что выделена и эксклюзивно принадлежит тесту.
    unsafe {
        new_owner_page.write_volatile(NEW_OWNER_VALUE);
    }

    // Прежний владелец получает ASID в начале поколения, и после следующего rollover этот ASID
    // достаётся одному из первых новых адресных пространств.
    let first_generation = unpack_generation(fresh_tag(factory));
    while unpack_generation(fresh_tag(factory)) == first_generation {}
    let owner_tag = previous_owner
        .handle()
        .expect("user variant has handle")
        .tag
        .raw();
    let spacer = AddressSpace::new_user(factory).expect("create spacer AS");
    let _ = spacer.handle();

    let (owner_value, observed) = loop {
        // Записи TLB прежнего владельца загружаются на том же CPU, где идёт чтение под его ASID.
        let irq_mask = DaifCriticalSection::enter();
        let owner_handle = previous_owner.handle().expect("user variant has handle");
        Aarch64Context::switch_address_space(Some(owner_handle));
        // SAFETY: PROBE_VA прежнего владельца отображён с UserRW.
        let owner_value = unsafe { (PROBE_VA as *const u64).read_volatile() };
        Aarch64Context::switch_address_space(None);
        // Rollover резервирует ASID последнего `acquire` этого CPU, а не прежнего владельца.
        let _ = spacer.handle();
        let space = AddressSpace::new_user(factory).expect("create AS");
        let handle = space.handle().expect("user variant has handle");
        if unpack_generation(handle.tag.raw()) != unpack_generation(owner_tag) {
            let observed = read_probe_under_reissued_asid(
                factory,
                space,
                handle,
                unpack_asid(owner_tag),
                new_owner_pa,
            );
            drop(irq_mask);
            break (owner_value, observed);
        }
        drop(irq_mask);
    };

    test_harness::check_eq!(owner_value, PREVIOUS_OWNER_VALUE);
    test_harness::check_eq!(observed, Some(NEW_OWNER_VALUE));
    drop(spacer);
    drop(previous_owner);
}

fn fresh_tag(factory: &dyn AddressSpaceFactory) -> u64 {
    let space = AddressSpace::new_user(factory).expect("create AS");
    space.handle().expect("user variant has handle").tag.raw()
}

fn read_probe_under_reissued_asid(
    factory: &dyn AddressSpaceFactory,
    mut space: Arc<AddressSpace>,
    mut handle: AddressSpaceHandle,
    owner_asid: u16,
    page: PageAlignedAddress,
) -> Option<u64> {
    loop {
        let asid = unpack_asid(handle.tag.raw());
        if asid > owner_asid {
            return None;
        }
        if asid == owner_asid {
            break;
        }
        space = AddressSpace::new_user(factory).expect("create AS");
        handle = space.handle().expect("user variant has handle");
    }
    let va = PageAlignedVirtualAddress::from_usize(PROBE_VA).expect("4K-aligned VA");
    space
        .mapper()
        .expect("user variant has mapper")
        .map_exact(va, page, PAGE_SIZE, user_rw_flags())
        .expect("map_exact probe page");
    Aarch64Context::switch_address_space(Some(handle));
    // SAFETY: PROBE_VA этого адресного пространства отображён с UserRW.
    let value = unsafe { (PROBE_VA as *const u64).read_volatile() };
    Aarch64Context::switch_address_space(None);
    Some(value)
}
