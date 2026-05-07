//! E2E проверка цепочки `MemoryMapper::remap` -> `Aarch64Context::init_user` ->
//! `eret` в EL0 -> SVC из EL0 -> существующий диспатчер.
//!
//! Сценарий:
//! 1. Аллоцируем 4К-страницу в kernel-heap (получаем kernel-VA + PA через
//!    higher-half линейное отображение).
//! 2. `map_exact` создаёт alias-маппинг 4К на отдельный VA в области выше
//!    kernel image (`USER_TEST_VA_BASE`) - там нет существующих block-mappings,
//!    поэтому L3 leaf создастся, и `remap` потом сработает.
//! 3. Через alias-VA пишем 2 инструкции `svc #TestEl0Probe; b .` и
//!    инвалидируем I-cache.
//! 4. Аналогично - для user-stack.
//! 5. `MemoryMapper::remap` меняет флаги на UserRX (payload) и UserRW (stack).
//!    EL0 ходит через TTBR1 на тот же kernel root - после `remap` страницы
//!    видны из EL0 с нужными правами.
//! 6. Spawn worker-thread; в его trampoline'е делаем `init_user` + `start` -
//!    управление уходит в EL0, payload делает SVC. Handler `TestEl0Probe`
//!    фиксирует `(arg0, origin)` в [`el0_probe`] и завершает thread.
//! 7. Главный test-thread ждёт записи и проверяет: `Origin::User` + `arg0`
//!    равен переданному в `init_user` `bootstrap_x0`.
//!
//! Проверки покрывают: `remap` корректно меняет AP/UXN; `init_user` правильно
//! пробрасывает x19/x20/x21; `el0_entry_shim` ставит SP_EL0/ELR_EL1/SPSR_EL1
//! и делает `eret`; vector `sync_lower_el_a64` ловит SVC и идёт в диспатчер
//! с `Origin::User`.
#![allow(unsafe_code)]

extern crate alloc;

use alloc::{boxed::Box, vec};
use core::{
    alloc::Layout,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use drivers_common::services::scheduler::{Priority, SchedulerServiceExt, SpawnConfig};
use main::{
    qemu_tests::el0_probe,
    sched::ArchContext,
    syscall_bridge::{memory_mapper, scheduler},
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
const KERNEL_STACK_SIZE: usize = 8 * 1024;

/// База для alias-маппингов test'а. Выбрана выше реального RAM mapping
/// (RAM в qemu-virt = 0x4000_0000..0x6000_0000 -> higher-half-mapped в
/// `HIGHER_HALF_BASE + 0x4000_0000..HIGHER_HALF_BASE + 0x6000_0000`).
/// 0x8000_0000 = +2 ГБ от base - заведомо в новом L1[2]-bucket'е, где никаких
/// block-mappings нет; `map_exact` создаст свежие L1/L2/L3-таблицы и L3-leaf,
/// `remap` потом сможет точечно поменять AP/UXN.
const USER_TEST_VA_BASE: usize = HIGHER_HALF_BASE + 0x8000_0000;
const USER_TEST_PAYLOAD_VA: usize = USER_TEST_VA_BASE;
const USER_TEST_STACK_VA: usize = USER_TEST_VA_BASE + PAGE_SIZE;

/// `svc #0xFF00` (`TestEl0Probe`) - encoded `0xD400_0001 | (imm16 << 5)`.
const SVC_TEST_EL0_PROBE: u32 = 0xD400_0001 | ((SyscallTestEl0Probe::IMM16 as u32) << 5);
/// `b .` - безусловный jump на текущий PC (offset 0).
const B_LOOP: u32 = 0x1400_0000;

/// Зеркало `SyscallOp::TestEl0Probe` для сборки SVC instruction-encoding.
/// Реальный enum-вариант доступен только при `feature = "qemu-tests"`, что
/// у нас уже включено для этого модуля.
struct SyscallTestEl0Probe;
impl SyscallTestEl0Probe {
    const IMM16: u16 = 0xFF00;
}

fn kernel_rw_flags() -> MemFlags {
    MemFlags::Private(Owners {
        kernel: PrivateMemoryPermission {
            access: AccessMode::Writable,
            executable: Executable::NotAllowed,
        },
        user: PrivateMemoryPermission {
            access: AccessMode::None,
            executable: Executable::NotAllowed,
        },
    })
}

fn user_rx_flags() -> MemFlags {
    MemFlags::Private(Owners {
        kernel: PrivateMemoryPermission {
            access: AccessMode::None,
            executable: Executable::NotAllowed,
        },
        user: PrivateMemoryPermission {
            access: AccessMode::Readonly,
            executable: Executable::Allowed,
        },
    })
}

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

/// Аллоцирует выровненную 4К-страницу из kernel-heap и зануляет её. Утечка
/// сделана намеренно: после `remap` память выходит из-под контроля Rust-аллокатора.
fn leak_aligned_page() -> *mut u8 {
    let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).expect("PAGE_SIZE valid layout");
    // SAFETY: layout валиден; ptr - свежевыделенный, занулённый, не shared.
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    assert!(!ptr.is_null(), "alloc_zeroed must succeed");
    ptr
}

/// Записывает payload-инструкции и синхронизирует I-cache, чтобы CPU прочитал
/// свежие байты как код.
///
/// SAFETY:
/// - `payload_ptr` указывает на валидную, занулённую 4К-страницу, эксклюзивно
///   принадлежащую вызывающему;
/// - страница выровнена на `PAGE_SIZE` (≥ 4 байт), поэтому каст к `*mut u32`
///   корректен.
unsafe fn write_payload(payload_ptr: *mut u8) {
    // 4К-выровненный указатель безопасно кастится к u32 (cast_ptr_alignment FP).
    #[allow(clippy::cast_ptr_alignment)]
    let words = payload_ptr.cast::<u32>();
    // SAFETY: caller гарантирует эксклюзивный, валидный, выровненный буфер.
    unsafe {
        words.add(0).write_volatile(SVC_TEST_EL0_PROBE);
        words.add(1).write_volatile(B_LOOP);

        // Очистка D-cache до PoU (point of unification) и инвалидация I-cache
        // - без этого CPU может выполнить устаревшие инструкции.
        core::arch::asm!(
            "dc cvau, {addr}",
            "dsb ish",
            "ic ivau, {addr}",
            "dsb ish",
            "isb",
            addr = in(reg) payload_ptr,
            options(nostack, preserves_flags),
        );
    }
}

/// Worker-thread'у нужно достать `kernel_stack_top` из closure'ы; делать это
/// через `static AtomicU64` проще, чем тащить `move`-захват указателя через
/// `unsafe Send` границу.
static KSTACK_TOP: AtomicU64 = AtomicU64::new(0);

/// Глобальный flag - тест запускается один раз. Если по какой-то причине
/// `el0_probe::peek()` уже True (например, кто-то ранее сигналил), мы должны
/// быть уверены, что наш payload поднял именно текущий probe.
static TEST_DONE: AtomicBool = AtomicBool::new(false);

/// Линейный physical address kernel-heap-страницы: heap замаплен через
/// `higher-half = PA + HIGHER_HALF_BASE`, поэтому `PA = VA - HIGHER_HALF_BASE`.
fn kheap_va_to_pa(kva: *mut u8) -> PageAlignedAddress {
    let pa = (kva as usize)
        .checked_sub(HIGHER_HALF_BASE)
        .expect("kheap pointer must be in higher-half range");
    PageAlignedAddress::from_usize(pa)
        .expect("4K-aligned heap allocation translates to 4K-aligned PA")
}

fn userspace_eret_to_el0_invokes_dispatcher() {
    el0_probe::reset();
    TEST_DONE.store(false, Ordering::Release);

    let mapper = memory_mapper();

    // Аллоцируем две физ.страницы из kernel-heap; их PA берём через
    // линейное higher-half отображение, kernel-VA для записи payload - это
    // ALIAS-VA, который мы создаём дальше через `map_exact`.
    let payload_kheap = leak_aligned_page();
    let stack_kheap = leak_aligned_page();
    let payload_pa = kheap_va_to_pa(payload_kheap);
    let stack_pa = kheap_va_to_pa(stack_kheap);

    let payload_alias = PageAlignedVirtualAddress::from_usize(USER_TEST_PAYLOAD_VA)
        .expect("USER_TEST_PAYLOAD_VA must be 4K-aligned");
    let stack_alias = PageAlignedVirtualAddress::from_usize(USER_TEST_STACK_VA)
        .expect("USER_TEST_STACK_VA must be 4K-aligned");

    // Шаг 1: alias-маппинг 4К с kernel-RW, чтобы kernel мог записать payload.
    mapper
        .map_exact(payload_alias, payload_pa, PAGE_SIZE, kernel_rw_flags())
        .expect("map_exact payload alias as KernelRW");
    mapper
        .map_exact(stack_alias, stack_pa, PAGE_SIZE, kernel_rw_flags())
        .expect("map_exact stack alias as KernelRW");

    // Шаг 2: пишем payload через alias-VA (та же физ.страница, kernel-RW).
    // SAFETY: `payload_alias` валидно замаплен на свежую страницу, kernel имеет RW.
    unsafe { write_payload(payload_alias.as_ptr::<u8>()) };

    // Шаг 3: смена AP/UXN - здесь и проверяется `MemoryMapper::remap`.
    mapper
        .remap(payload_alias, PAGE_SIZE, user_rx_flags())
        .expect("remap payload alias to UserRX");
    mapper
        .remap(stack_alias, PAGE_SIZE, user_rw_flags())
        .expect("remap stack alias to UserRW");

    // Sanity-check: leaf-биты payload-страницы должны быть UserRO+UXN=0+PXN=1+AF.
    let payload_raw = mapper
        .query_l3_raw(payload_alias)
        .expect("payload leaf must exist after remap");
    qemu_test_harness::kassert_eq!(payload_raw & 0b11, 0b11); // page desc
    qemu_test_harness::kassert_eq!((payload_raw >> 6) & 0b11, 0b11); // AP=UserRO
    qemu_test_harness::kassert_eq!((payload_raw >> 10) & 1, 1); // AF=1
    qemu_test_harness::kassert_eq!((payload_raw >> 53) & 1, 1); // PXN=1
    qemu_test_harness::kassert_eq!((payload_raw >> 54) & 1, 0); // UXN=0

    let stack_raw = mapper
        .query_l3_raw(stack_alias)
        .expect("stack leaf must exist after remap");
    qemu_test_harness::kassert_eq!((stack_raw >> 6) & 0b11, 0b01); // AP=UserRW
    qemu_test_harness::kassert_eq!((stack_raw >> 54) & 1, 1); // UXN=1

    let user_stack_top = (USER_TEST_STACK_VA + PAGE_SIZE) & !0xF;

    // Отдельный kernel stack для будущих SVC из EL0: после `eret` он останется
    // в SP_EL1, exception entry на следующем SVC будет работать на нём.
    let kstack_box: Box<[u8]> = vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice();
    let kstack_raw = Box::leak(kstack_box);
    let kstack_top_addr = (kstack_raw.as_mut_ptr() as usize + KERNEL_STACK_SIZE) & !0xF;
    KSTACK_TOP.store(kstack_top_addr as u64, Ordering::Relaxed);

    // Заглушаем неиспользуемые переменные (kheap-pointers нужны были только для PA-вычисления).
    let _ = (payload_kheap, stack_kheap);
    let user_pc = USER_TEST_PAYLOAD_VA;
    let bootstrap_x0: u64 = 0xCAFE_BABE_DEAD_BEEF;

    scheduler()
        .spawn(
            SpawnConfig::new("el0-worker").priority(Priority::highest()),
            move || {
                let kstack_top = KSTACK_TOP.load(Ordering::Relaxed) as *mut u8;
                let kstack_nn = NonNull::new(kstack_top).expect("kstack non-null");
                let ctx =
                    Aarch64Context::init_user(kstack_nn, user_pc, user_stack_top, bootstrap_x0);
                // SAFETY: ctx инициализирован init_user; start() безусловно
                // прыгает в наш el0_entry_shim -> eret в EL0. Worker-thread
                // после этого "превращается" в EL0-thread; его завершит
                // handler TestEl0Probe через scheduler.exit().
                unsafe { Aarch64Context::start(&ctx) }
            },
        )
        .expect("spawn el0-worker");

    let mut spins = 0;
    while el0_probe::peek().is_none() {
        scheduler().sleep_ms(20);
        spins += 1;
        assert!(spins <= 250, "EL0 probe didn't fire after 5s");
    }

    let (observed_x0, is_user) = el0_probe::peek().expect("peek must be Some after spin");
    qemu_test_harness::kassert!(is_user);
    qemu_test_harness::kassert_eq!(observed_x0, bootstrap_x0);
    TEST_DONE.store(true, Ordering::Release);
}

register_test!(
    USERSPACE_ERET_TO_EL0_INVOKES_DISPATCHER,
    "userspace_eret_to_el0_invokes_dispatcher",
    userspace_eret_to_el0_invokes_dispatcher
);
