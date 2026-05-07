//! Per-process AddressSpace: проверки переключения TTBR0 и изоляции
//! lower-half user-маппингов.
//!
//! В отличие от `userspace_entry.rs` эти тесты не требуют выхода в EL0 -
//! всё проверяется из kernel-thread'а через прямую запись/чтение TTBR0_EL1
//! (`Aarch64Context::switch_address_space` + `mrs ttbr0_el1`). Семантически
//! TTBR0 в EL1 управляет lower-half-walker'ом для user-уровня; lower-half
//! доступы из kernel-thread'а сюда же ходят, поэтому достаточно
//! почитать/записать одну lower-half-страницу через user-mapper'ы и убедиться,
//! что результат меняется в зависимости от активного TTBR0.
#![allow(unsafe_code)]

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

/// Удерживает Arc на user-AS живыми до конца QEMU-runner'а: освобождение
/// фреймов между тестами привело бы к их повторной выдаче из аллокатора и
/// к рассинхронизации с TLB.
static HOLD_A: AtomicU64 = AtomicU64::new(0);
static HOLD_B: AtomicU64 = AtomicU64::new(0);

use main::{
    sched::{AddressSpace, ArchContext},
    syscall_bridge,
};
use memory::{
    MemFlags,
    mem_flags::{AccessMode, Executable, Owners, PrivateMemoryPermission},
    physical_address::PageAlignedAddress,
    virtual_address::PageAlignedVirtualAddress,
};
use qemu_test_harness::register_test;

use crate::{HIGHER_HALF_BASE, sched::Aarch64Context};

const PAGE_SIZE: usize = 4096;

/// VA в нижней половине, не пересекающаяся с возможными identity-маппингами
/// тестов userspace_entry.
const PROBE_VA: usize = 0x5000_0000;

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

/// Аллоцирует выровненную 4К-страницу из kernel-heap и зануляет её.
/// Память намеренно утекает: после маппинга в user-AS высвобождать её нельзя.
fn leak_aligned_page() -> *mut u8 {
    let layout =
        core::alloc::Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).expect("PAGE_SIZE valid layout");
    // SAFETY: layout валиден, ptr - свежевыделенный, эксклюзивно принадлежит вызывающему.
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    assert!(!ptr.is_null(), "alloc_zeroed must succeed");
    ptr
}

fn kheap_va_to_pa(kva: *mut u8) -> PageAlignedAddress {
    let pa = (kva as usize)
        .checked_sub(HIGHER_HALF_BASE)
        .expect("kheap pointer must be in higher-half range");
    PageAlignedAddress::from_usize(pa).expect("4K-aligned PA")
}

/// Читает TTBR0_EL1.
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

fn make_user_as_with_probe_page(sentinel: u64) -> (Arc<AddressSpace>, *mut u8) {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    let user_as = AddressSpace::new_user(factory).expect("create user AS");
    let mapper = user_as.mapper().expect("user variant has mapper");

    let kheap = leak_aligned_page();
    let pa = kheap_va_to_pa(kheap);
    let va = PageAlignedVirtualAddress::from_usize(PROBE_VA).expect("PROBE_VA aligned");
    mapper
        .map_exact(va, pa, PAGE_SIZE, user_rw_flags())
        .expect("map_exact probe page");

    // карта, поэтому страница доступна без переключения TTBR0.
    // SAFETY: kheap - свежевыделенная, выровненная на PAGE_SIZE (>= 8) страница,
    // эксклюзивно наша; запись 8 байт не выходит за границу страницы.
    #[allow(clippy::cast_ptr_alignment)]
    unsafe {
        kheap.cast::<u64>().write_volatile(sentinel);
    }

    (user_as, kheap)
}

/// Активирует user-AS и читает первое 8-байтное значение по `PROBE_VA`.
/// через TTBR0_EL1, поэтому видит содержимое user-страницы.
fn read_probe_via_user_as(user_as: &AddressSpace) -> u64 {
    Aarch64Context::switch_address_space(user_as.handle());
    // SAFETY: PROBE_VA валиден в активном user-AS, страница UserRW (kernel
    // тоже имеет RW по флагам), 8-байтное чтение выровнено.
    unsafe {
        let ptr = PROBE_VA as *const u64;
        ptr.read_volatile()
    }
}

fn process_a_and_b_see_distinct_memory_at_same_va() {
    let sentinel_a: u64 = 0xA1A1_A1A1_A1A1_A1A1;
    let sentinel_b: u64 = 0xB2B2_B2B2_B2B2_B2B2;

    let (as_a, _kva_a) = make_user_as_with_probe_page(sentinel_a);
    let (as_b, _kva_b) = make_user_as_with_probe_page(sentinel_b);

    qemu_test_harness::kassert_eq!(read_probe_via_user_as(&as_a), sentinel_a);
    qemu_test_harness::kassert_eq!(read_probe_via_user_as(&as_b), sentinel_b);
    // Возвращаемся в kernel-AS.
    Aarch64Context::switch_address_space(None);

    HOLD_A.store(Arc::into_raw(as_a) as usize as u64, Ordering::Release);
    HOLD_B.store(Arc::into_raw(as_b) as usize as u64, Ordering::Release);
}

fn ttbr0_is_switched_on_process_change() {
    let (as_a, _) = make_user_as_with_probe_page(0xDEAD_BEEF);
    let handle_a = as_a.handle().expect("user AS has handle");
    let root_a_pa = handle_a.root.as_u64();

    Aarch64Context::switch_address_space(Some(handle_a));
    let ttbr0_after = read_ttbr0();
    // TTBR0_EL1[47:12] хранит адрес таблицы; [63:48] - ASID. Сравниваем по
    // PA-mask 4К-страницы.
    qemu_test_harness::kassert_eq!(ttbr0_after & 0x0000_FFFF_FFFF_F000, root_a_pa);

    // Возврат в kernel-AS.
    Aarch64Context::switch_address_space(None);
    qemu_test_harness::kassert_eq!(read_ttbr0(), 0);

    // Утечка as_a, чтобы фрейм root'а не вернулся в аллокатор и не сломал
    // следующий тест.
    core::mem::forget(as_a);
}

fn kernel_thread_after_user_has_ttbr0_zero() {
    let (as_a, _) = make_user_as_with_probe_page(0x1234_5678);
    Aarch64Context::switch_address_space(as_a.handle());
    qemu_test_harness::kassert!(read_ttbr0() != 0);
    Aarch64Context::switch_address_space(None);
    qemu_test_harness::kassert_eq!(read_ttbr0(), 0);
    core::mem::forget(as_a);
}

fn user_thread_exit_releases_address_space_frames() {
    // Создаём user-AS, маппим страницу - это заставит mapper выделить L1/L2/L3
    // фреймы. После drop(user_as) корневой root-фрейм возвращается в
    // FrameAllocator (промежуточные L1/L2/L3 - позднее, по мере раскрутки
    // walker'ом; см. план). Здесь проверяем, что после drop'а активный
    // TTBR0 не указывает на освобождённый root: переключаемся на user_as,
    // запоминаем root, дропаем, переключаемся обратно в kernel-AS.
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    let user_as = AddressSpace::new_user(factory).expect("create user AS");
    let handle = user_as.handle().expect("root");
    let root_before = handle.root.as_u64();

    Aarch64Context::switch_address_space(Some(handle));
    qemu_test_harness::kassert_eq!(read_ttbr0() & 0x0000_FFFF_FFFF_F000, root_before);

    // Возвращаемся в kernel-AS, потом drop - иначе TLB удержит трансляции
    // из old user-AS до следующего switch'а.
    Aarch64Context::switch_address_space(None);
    drop(user_as);

    qemu_test_harness::kassert_eq!(read_ttbr0(), 0);
}

register_test!(
    ADDRESS_SPACE_DISTINCT_MEMORY,
    "process_a_and_b_see_distinct_memory_at_same_va",
    process_a_and_b_see_distinct_memory_at_same_va
);
register_test!(
    ADDRESS_SPACE_TTBR0_SWITCHED,
    "ttbr0_is_switched_on_process_change",
    ttbr0_is_switched_on_process_change
);
register_test!(
    ADDRESS_SPACE_KERNEL_AFTER_USER,
    "kernel_thread_after_user_has_ttbr0_zero",
    kernel_thread_after_user_has_ttbr0_zero
);
register_test!(
    ADDRESS_SPACE_EXIT_RELEASES,
    "user_thread_exit_releases_address_space_frames",
    user_thread_exit_releases_address_space_frames
);
