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
use scheduler::{
    AddressSpace, ArchContext, Priority, SchedulerServiceExt, SpawnAddressSpace, SpawnConfig,
    UserBootstrapArg, UserEntry,
};
use spin::Once;
use syscall::SyscallOp;
use test_harness_qemu::register_test;
use test_harness_qemu_aarch64::payload::{B_LOOP, Instruction, Reg, movz_x, svc};

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

const fn svc_op(op: SyscallOp) -> Instruction {
    svc(op as u16)
}

fn build_payload() -> [Instruction; 6] {
    [
        movz_x(Reg::X1, EVENT_SIGNALED as u16, 0),
        movz_x(Reg::X2, 0, 0),
        svc_op(SyscallOp::ObjectSignal),
        movz_x(Reg::X0, 0, 0),
        svc_op(SyscallOp::ThreadExit),
        B_LOOP,
    ]
}

/// Свежий 4К-фрейм + его kernel-VA через линейную PA->VA-карту. Намеренная
/// утечка: после `remap` страница выходит из-под контроля Rust-аллокатора.
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
    let payload = build_payload();
    // SAFETY: caller гарантирует эксклюзивный, валидный, выровненный буфер.
    unsafe {
        for (idx, instruction) in payload.iter().enumerate() {
            words.add(idx).write_volatile(instruction.word());
        }

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

    let (payload_kheap, payload_pa) = allocate_probe_page();
    let (stack_kheap, stack_pa) = allocate_probe_page();

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
    test_harness_qemu::kassert_eq!(payload_raw & 0b11, 0b11);
    test_harness_qemu::kassert_eq!((payload_raw >> 6) & 0b11, 0b11); // AP=UserRO
    test_harness_qemu::kassert_eq!((payload_raw >> 10) & 1, 1); // AF=1
    test_harness_qemu::kassert_eq!((payload_raw >> 53) & 1, 1); // PXN=1
    test_harness_qemu::kassert_eq!((payload_raw >> 54) & 1, 0); // UXN=0

    let stack_raw = aarch64_mapper
        .query_leaf_raw(stack_va)
        .expect("stack leaf must exist");
    test_harness_qemu::kassert_eq!((stack_raw >> 6) & 0b11, 0b01); // AP=UserRW
    test_harness_qemu::kassert_eq!((stack_raw >> 54) & 1, 1); // UXN=1

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
