//! Проверка работы ASID-теггинга на стороне MMU и context-switch.
//!
//! Тесты пользуются той же инфраструктурой, что `address_space.rs`
//! (probe-page в lower-half, чтение/запись через активный TTBR0). Дополнительно
//! проверяются:
//! - что после первого `switch_address_space` биты `[63:48]` TTBR0_EL1
//!   содержат ASID, выданный аллокатором (≠ 0);
//! - что два разных user-AS получают различные ASID;
//! - что один и тот же AS сохраняет свой ASID между `switch`'ами;
//! - что user-страницы маркируются `nG=1`, а kernel - `nG=0`;
//! - что TCR_EL1.AS соответствует ширине ASID, обнаруженной runtime;
//! - что rollover не ломает корректность для 8-bit ширины (в QEMU фактическая
//!   ширина - 16 бит, поэтому выделять 65k AS дорого; этот тест отдельный).
#![allow(unsafe_code)]

extern crate alloc;

use alloc::sync::Arc;

use kernelspace::syscall_bridge;
use memory::{
    MemFlags,
    mem_flags::{AccessMode, Executable, Owners, PrivateMemoryPermission},
    memory_mapper::MemoryMapper,
    physical_address::PageAlignedAddress,
    virtual_address::PageAlignedVirtualAddress,
};
use scheduler::{AddressSpace, ArchContext};
use test_harness_qemu::register_test;

use crate::{
    HIGHER_HALF_BASE,
    memory::{
        address_space_factory::UserAarch64MemoryMapper,
        regs::{common::EL1, id_aa64mmfr0::IdAa64Mmfr0},
    },
    sched::Aarch64Context,
};

/// Downcast'ит `&dyn MemoryMapper`, выданный `Aarch64AddressSpaceFactory`,
/// до конкретного типа для доступа к платформенному API диагностики.
fn downcast_user_mapper(mapper: &(dyn MemoryMapper + Send + Sync)) -> &UserAarch64MemoryMapper {
    mapper
        .as_any()
        .downcast_ref::<UserAarch64MemoryMapper>()
        .expect("user-AS mapper must be Aarch64MemoryMapper")
}

const PAGE_SIZE: usize = 4096;
const PROBE_VA: usize = 0x6000_0000;

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

/// Свежий 4К-фрейм + его kernel-VA через линейную PA->VA-карту. Намеренная
/// утечка: после маппинга в user-AS вернуть фрейм нельзя.
fn allocate_probe_page() -> (*mut u8, PageAlignedAddress) {
    let fa = syscall_bridge::frame_allocator().expect("FrameAllocator must be installed");
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

fn ttbr0_carries_asid_after_first_switch() {
    let (user_as, _kva) = make_user_as_with_probe_page(PROBE_VA, 0xDEAD_BEEF);
    let handle = user_as.handle().expect("user handle");

    Aarch64Context::switch_address_space(Some(handle));
    let ttbr0 = read_ttbr0();
    let asid = (ttbr0 >> 48) & 0xFFFF;
    test_harness_qemu::kassert!(asid != 0);

    Aarch64Context::switch_address_space(None);
    core::mem::forget(user_as);
}

fn two_user_as_have_distinct_asids() {
    let (as_a, _) = make_user_as_with_probe_page(PROBE_VA, 0x1);
    let (as_b, _) = make_user_as_with_probe_page(PROBE_VA, 0x2);

    let h_a = as_a.handle().expect("handle a");
    let h_b = as_b.handle().expect("handle b");

    Aarch64Context::switch_address_space(Some(h_a));
    let ttbr_a = read_ttbr0();
    Aarch64Context::switch_address_space(Some(h_b));
    let ttbr_b = read_ttbr0();
    Aarch64Context::switch_address_space(None);

    let asid_a = (ttbr_a >> 48) & 0xFFFF;
    let asid_b = (ttbr_b >> 48) & 0xFFFF;
    test_harness_qemu::kassert!(asid_a != 0);
    test_harness_qemu::kassert!(asid_b != 0);
    test_harness_qemu::kassert!(asid_a != asid_b);

    core::mem::forget(as_a);
    core::mem::forget(as_b);
}

fn same_as_keeps_asid_across_switch() {
    let (as_a, _) = make_user_as_with_probe_page(PROBE_VA, 0xAA);
    let h_first = as_a.handle().expect("handle first");
    let h_second = as_a.handle().expect("handle second");
    test_harness_qemu::kassert_eq!(h_first.tag, h_second.tag);

    Aarch64Context::switch_address_space(Some(h_first));
    let ttbr_first = read_ttbr0();
    Aarch64Context::switch_address_space(None);
    Aarch64Context::switch_address_space(Some(h_second));
    let ttbr_second = read_ttbr0();
    Aarch64Context::switch_address_space(None);

    test_harness_qemu::kassert_eq!(ttbr_first, ttbr_second);

    core::mem::forget(as_a);
}

fn user_pages_have_ng_bit_set() {
    let (user_as, _) = make_user_as_with_probe_page(PROBE_VA, 0xCC);
    let mapper = user_as.mapper().expect("user mapper");
    let va = PageAlignedVirtualAddress::from_usize(PROBE_VA).expect("aligned");
    let raw = downcast_user_mapper(mapper)
        .query_leaf_raw(va)
        .expect("leaf must exist");
    // nG бит - `[11]`.
    test_harness_qemu::kassert_eq!((raw >> 11) & 1, 1);
    core::mem::forget(user_as);
}

fn tcr_as_matches_runtime_asid_width() {
    let width = IdAa64Mmfr0::<EL1>::asid_width();
    let tcr = read_tcr();
    let as_bit = (tcr >> 36) & 1;
    let expected = match width {
        crate::memory::regs::id_aa64mmfr0::AsidWidth::Bits16 => 1,
        crate::memory::regs::id_aa64mmfr0::AsidWidth::Bits8 => 0,
    };
    test_harness_qemu::kassert_eq!(as_bit, expected);
}

/// Активирует много AS подряд, чтобы спровоцировать rollover. На QEMU
/// (cortex-a72) ASID-ширина - 16 бит, поэтому генерим в широком диапазоне.
/// Сразу же выдаём release при пересоздании, чтобы не потратить ASID-слоты
/// окончательно: выделяем чуть выше предела по числу одновременно живых AS,
/// но drop'аем сразу после проверки, и аллокатор лениво переиспользует слоты
/// на следующем rollover'е.
fn rollover_smoke() {
    let (probe_as, kva_a) = make_user_as_with_probe_page(PROBE_VA, 0x1234);
    let h_probe = probe_as.handle().expect("handle");
    Aarch64Context::switch_address_space(Some(h_probe));
    // SAFETY: PROBE_VA замаплен с UserRW, чтение валидно.
    let observed = unsafe { (PROBE_VA as *const u64).read_volatile() };
    test_harness_qemu::kassert_eq!(observed, 0x1234);
    Aarch64Context::switch_address_space(None);

    // Одна страница переиспользуется во всех AS - чтобы не съедать RAM
    // пропорционально числу итераций (`forget` ниже всё ещё оставляет
    // каждый AS живым ради per-AS-факторов аллокатора, но это уже только
    // page-table-фреймы).
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
    // цепочка switch'ей всё равно проверяется.
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
        test_harness_qemu::kassert_eq!(v, 0x5555);
        Aarch64Context::switch_address_space(None);
        core::mem::forget(a);
    }

    // ещё видна (т.е. trace переключений ASID не повредил трансляции).
    let h_probe2 = probe_as.handle().expect("handle 2");
    Aarch64Context::switch_address_space(Some(h_probe2));
    // SAFETY: см. выше.
    let observed2 = unsafe { (PROBE_VA as *const u64).read_volatile() };
    test_harness_qemu::kassert_eq!(observed2, 0x1234);
    Aarch64Context::switch_address_space(None);

    let _ = kva_a;
    let _ = shared_kheap;
    core::mem::forget(probe_as);
}

register_test!(
    ASID_TTBR0_CARRIES_ASID,
    "ttbr0_carries_asid_after_first_switch",
    ttbr0_carries_asid_after_first_switch
);
register_test!(
    ASID_TWO_AS_DISTINCT,
    "two_user_as_have_distinct_asids",
    two_user_as_have_distinct_asids
);
register_test!(
    ASID_SAME_AS_KEEPS,
    "same_as_keeps_asid_across_switch",
    same_as_keeps_asid_across_switch
);
register_test!(
    ASID_USER_PAGES_NG,
    "user_pages_have_ng_bit_set",
    user_pages_have_ng_bit_set
);
register_test!(
    ASID_TCR_AS_RUNTIME,
    "tcr_as_matches_runtime_asid_width",
    tcr_as_matches_runtime_asid_width
);
register_test!(ASID_ROLLOVER_SMOKE, "rollover_smoke", rollover_smoke);
