//! E2E проверка цепочки `MemoryMapper::remap` -> `Aarch64Context::init_user` ->
//! `eret` в EL0 -> SVC из EL0 -> существующий диспатчер.
//!
//! Сценарий:
//! 1. Через `AddressSpaceFactory` создаём отдельный user-AS с собственным
//!    L0-root (TTBR0).
//! 2. Аллоцируем 4К-страницу в kernel-heap (получаем kernel-VA + PA через
//!    higher-half линейное отображение).
//! 3. `map_exact` через user-mapper создаёт lower-half-маппинг 4К на VA
//!    `USER_TEST_PAYLOAD_VA = 0x4000_0000`. Через kernel-VA пишем payload
//!    `ObjectSignal; ThreadExit` и инвалидируем I-cache.
//! 4. Аналогично - для user-stack на `USER_TEST_PAYLOAD_VA + PAGE_SIZE`.
//! 5. `MemoryMapper::remap` меняет флаги на UserRX (payload) и UserRW (stack).
//! 6. Spawn worker-thread; в его trampoline'е переключаем TTBR0 на user-AS,
//!    далее `init_user` + `start` - управление уходит в EL0, payload делает SVC.
//! 7. Главный test-thread ждёт сигнал на Event, handle которого был передан
//!    в `init_user` через `bootstrap_x0`.
//!
//! По сравнению с прежней версией убран alias-маппинг через higher-half:
//! payload и stack живут в нижней половине user-AS, как и положено user-страницам.
#![allow(unsafe_code)]

extern crate alloc;

use alloc::{boxed::Box, sync::Arc, vec};
use core::{
    alloc::Layout,
    ptr::NonNull,
    sync::atomic::{AtomicU64, Ordering},
};

use kernelspace::syscall_bridge;
use kobject::{EVENT_SIGNALED, Event, Handle, KObject, Rights, install_handle};
use memory::{
    MemFlags,
    memory_mapper::MemoryMapper,
    physical_address::PageAlignedAddress,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use qemu_test_harness::register_test;
use scheduler::{
    AddressSpace, ArchContext, Priority, SchedulerServiceExt, SpawnAddressSpace, SpawnConfig,
    UserBootstrapArg, UserEntry,
};
use spin::Once;
use syscall::SyscallOp;

use crate::{
    HIGHER_HALF_BASE, memory::address_space_factory::UserAarch64MemoryMapper, sched::Aarch64Context,
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
const KERNEL_STACK_SIZE: usize = 8 * 1024;

/// Lower-half VA для payload в user-AS. Любая страница ниже kernel image -
/// в чистом user-AS таблиц нет, `map_exact` создаёт свежую цепочку L1/L2/L3.
const USER_TEST_PAYLOAD_VA: usize = 0x4000_0000;
const USER_TEST_STACK_VA: usize = USER_TEST_PAYLOAD_VA + PAGE_SIZE;

const SVC_OBJECT_SIGNAL: u32 = svc(SyscallOp::ObjectSignal);
const SVC_THREAD_EXIT: u32 = svc(SyscallOp::ThreadExit);
const MOVZ_X0_ZERO: u32 = 0xD280_0000;
const MOVZ_X1_EVENT_SIGNALED: u32 = 0xD280_0021;
const MOVZ_X2_ZERO: u32 = 0xD280_0002;
/// `b .` - безусловный jump на текущий PC (offset 0).
const B_LOOP: u32 = 0x1400_0000;

const fn svc(op: SyscallOp) -> u32 {
    0xD400_0001 | ((op as u32) << 5)
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

/// Записывает payload-инструкции через kernel-VA (higher-half линейное
/// отображение) и синхронизирует I-cache, чтобы CPU прочитал свежие байты
/// как код, когда EL0 будет к ним обращаться.
///
/// SAFETY:
/// - `kernel_va` указывает на валидную, занулённую 4К-страницу,
///   эксклюзивно принадлежащую вызывающему;
/// - страница выровнена на `PAGE_SIZE` (≥ 4 байт), поэтому каст к `*mut u32`
///   корректен.
unsafe fn write_payload(kernel_va: *mut u8) {
    #[allow(clippy::cast_ptr_alignment)]
    let words = kernel_va.cast::<u32>();
    // SAFETY: caller гарантирует эксклюзивный, валидный, выровненный буфер.
    unsafe {
        words.add(0).write_volatile(MOVZ_X1_EVENT_SIGNALED);
        words.add(1).write_volatile(MOVZ_X2_ZERO);
        words.add(2).write_volatile(SVC_OBJECT_SIGNAL);
        words.add(3).write_volatile(MOVZ_X0_ZERO);
        words.add(4).write_volatile(SVC_THREAD_EXIT);
        words.add(5).write_volatile(B_LOOP);

        core::arch::asm!(
            "dc cvau, {addr}",
            "dsb ish",
            "ic ivau, {addr}",
            "dsb ish",
            "isb",
            addr = in(reg) kernel_va,
            options(nostack, preserves_flags),
        );
    }
}

static KSTACK_TOP: AtomicU64 = AtomicU64::new(0);
static USER_AS_ROOT: AtomicU64 = AtomicU64::new(0);

/// Удерживает Arc на user-AS до конца теста: иначе при завершении spawn-замыкания
/// AS освободится и мы потеряем root-фрейм.
static USER_AS_HOLDER: Once<Arc<AddressSpace>> = Once::new();

/// Линейный physical address kernel-heap-страницы: heap замаплен через
/// `higher-half = PA + HIGHER_HALF_BASE`, поэтому `PA = VA - HIGHER_HALF_BASE`.
fn kheap_va_to_pa(kva: *mut u8) -> PageAlignedAddress {
    let pa = (kva as usize)
        .checked_sub(HIGHER_HALF_BASE)
        .expect("kheap pointer must be in higher-half range");
    PageAlignedAddress::from_usize(pa)
        .expect("4K-aligned heap allocation translates to 4K-aligned PA")
}

#[allow(clippy::similar_names)]
fn userspace_eret_to_el0_invokes_dispatcher() {
    let event = Event::new();
    let handle = Handle::new(KObject::Event(event.clone()), Rights::SIGNAL);
    let event_handle = install_handle(handle).expect("install bootstrap Event handle");
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    let user_as = AddressSpace::new_user(factory).expect("create user AS");
    let user_mapper = user_as.mapper().expect("user variant has mapper");

    USER_AS_ROOT.store(
        user_as.handle().expect("user handle").root.as_u64(),
        Ordering::Release,
    );
    USER_AS_HOLDER.call_once(|| user_as.clone());

    let payload_kheap = leak_aligned_page();
    let stack_kheap = leak_aligned_page();
    let payload_pa = kheap_va_to_pa(payload_kheap);
    let stack_pa = kheap_va_to_pa(stack_kheap);

    let payload_va = PageAlignedVirtualAddress::from_usize(USER_TEST_PAYLOAD_VA)
        .expect("USER_TEST_PAYLOAD_VA must be 4K-aligned");
    let stack_va = PageAlignedVirtualAddress::from_usize(USER_TEST_STACK_VA)
        .expect("USER_TEST_STACK_VA must be 4K-aligned");

    // Шаг 1: маппинг 4К с user-RW, чтобы сначала kernel смог записать payload
    // через kernel-VA (higher-half), а затем remap'нуть в user-RX.
    user_mapper
        .map_exact(payload_va, payload_pa, PAGE_SIZE, MemFlags::user_rw())
        .expect("map_exact payload as UserRW");
    user_mapper
        .map_exact(stack_va, stack_pa, PAGE_SIZE, MemFlags::user_rw())
        .expect("map_exact stack as UserRW");

    // Шаг 2: пишем payload через kernel-VA (та же физ.страница). Lower-half
    // user-AS ещё не активен в текущем CPU (kernel-thread с TTBR0=0), но
    // higher-half линейная карта работает всегда.
    // SAFETY: payload_kheap валиден, занулён и эксклюзивно принадлежит тесту.
    unsafe { write_payload(payload_kheap) };

    // Шаг 3: смена флагов payload-страницы на UserRX, stack остаётся UserRW.
    user_mapper
        .remap(payload_va, PAGE_SIZE, MemFlags::user_rx())
        .expect("remap payload to UserRX");

    // Sanity-check leaf-битов через user-AS mapper.
    let aarch64_mapper = downcast_user_mapper(user_mapper);
    let payload_raw = aarch64_mapper
        .query_leaf_raw(payload_va)
        .expect("payload leaf must exist after remap");
    qemu_test_harness::kassert_eq!(payload_raw & 0b11, 0b11);
    qemu_test_harness::kassert_eq!((payload_raw >> 6) & 0b11, 0b11); // AP=UserRO
    qemu_test_harness::kassert_eq!((payload_raw >> 10) & 1, 1); // AF=1
    qemu_test_harness::kassert_eq!((payload_raw >> 53) & 1, 1); // PXN=1
    qemu_test_harness::kassert_eq!((payload_raw >> 54) & 1, 0); // UXN=0

    let stack_raw = aarch64_mapper
        .query_leaf_raw(stack_va)
        .expect("stack leaf must exist");
    qemu_test_harness::kassert_eq!((stack_raw >> 6) & 0b11, 0b01); // AP=UserRW
    qemu_test_harness::kassert_eq!((stack_raw >> 54) & 1, 1); // UXN=1

    let user_stack_top = (USER_TEST_STACK_VA + PAGE_SIZE) & !0xF;

    let kstack_box: Box<[u8]> = vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice();
    let kstack_raw = Box::leak(kstack_box);
    let kstack_top_addr = (kstack_raw.as_mut_ptr() as usize + KERNEL_STACK_SIZE) & !0xF;
    KSTACK_TOP.store(kstack_top_addr as u64, Ordering::Relaxed);

    let _ = (payload_kheap, stack_kheap);
    let user_pc = USER_TEST_PAYLOAD_VA;
    let bootstrap_x0: u64 = u64::from(event_handle.raw().get());

    syscall_bridge::scheduler()
        .spawn(
            SpawnConfig::new("el0-worker")
                .priority(Priority::highest())
                .address_space(SpawnAddressSpace::Inherit),
            move || {
                let kstack_top = KSTACK_TOP.load(Ordering::Relaxed) as *mut u8;
                let kstack_nn = NonNull::new(kstack_top).expect("kstack non-null");
                // Перед `eret` активируем user-AS. Scheduler сам этого не
                // сделал бы - worker запущен как kernel-thread (Kernel-AS),
                // поэтому берём handle напрямую у владеемого AS.
                let user_as_arc = USER_AS_HOLDER
                    .get()
                    .expect("user AS holder must be set before worker runs");
                Aarch64Context::switch_address_space(user_as_arc.handle());
                let ctx = Aarch64Context::init_user(UserEntry {
                    kernel_stack_top: kstack_nn,
                    user_pc: VirtualAddress::new(user_pc),
                    user_sp: VirtualAddress::new(user_stack_top),
                    arg: UserBootstrapArg(bootstrap_x0),
                });
                // SAFETY: ctx инициализирован init_user; start() безусловно
                // прыгает в наш el0_entry_shim -> eret в EL0.
                unsafe { Aarch64Context::start(&ctx) }
            },
        )
        .expect("spawn el0-worker");

    let mut spins = 0;
    while event.peek() & EVENT_SIGNALED == 0 {
        syscall_bridge::scheduler().sleep_ms(20);
        spins += 1;
        assert!(spins <= 250, "EL0 payload didn't signal after 5s");
    }
}

register_test!(
    USERSPACE_ERET_TO_EL0_INVOKES_DISPATCHER,
    "userspace_eret_to_el0_invokes_dispatcher",
    userspace_eret_to_el0_invokes_dispatcher
);
