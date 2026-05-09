//! E2E проверка `SchedulerService::spawn_user_process`.
//!
//! В отличие от низкоуровневого теста ручного входа в user-mode,
//! этот тест не делает ручной setup'а user-AS, payload-маппинга и `init_user`
//! - он использует публичный API `UserImage` + `spawn_user_process`. После
//! успеха scheduler сам:
//!   1) создаёт user-AS через factory,
//!   2) `load_user_image` маппит сегменты + стек через `MemoryMapper::map_owned`
//!      (платформенная реализация выполняет нужную синхронизацию exec-страниц),
//!   3) инициализирует user entry через `ArchContext::init_user(UserEntry)`,
//!   4) ставит в ready-queue.
//!
//! Тест передаёт payload'у bootstrap-handle на `Event`, ждёт сигнал от
//! `ObjectSignal`, затем payload делает `ThreadExit`.

use alloc::vec;

use kobject::{EVENT_SIGNALED, Event, Handle, KObject, Rights};
use memory::{
    MemFlags,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use qemu_test_harness::register_test;
use scheduler::{Priority, SchedulerServiceExt, UserProcessLaunch};
use syscall::SyscallOp;
use userspace::{UserImage, UserSegment};

const PAGE_SIZE: usize = 4096;
/// Lower-half VA для payload - чистый user-AS, никаких пересечений.
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
/// Минимальный footprint: 1 страница стека достаточна, payload не пишет в стек.
const USER_STACK_SIZE: usize = PAGE_SIZE;

const SVC_OBJECT_SIGNAL: u32 = svc(SyscallOp::ObjectSignal);
const SVC_THREAD_EXIT: u32 = svc(SyscallOp::ThreadExit);

const MOVZ_X0_ZERO: u32 = 0xD280_0000;
const MOVZ_X1_EVENT_SIGNALED: u32 = 0xD280_0021;
const MOVZ_X2_ZERO: u32 = 0xD280_0002;
const B_LOOP: u32 = 0x1400_0000;

const fn svc(op: SyscallOp) -> u32 {
    0xD400_0001 | ((op as u32) << 5)
}

/// Сборка байт-кода: `ObjectSignal(x0, EVENT_SIGNALED, 0); ThreadExit(0); b .`.
/// `b .` - fallback на случай возврата (не должен исполниться).
fn build_payload() -> [u8; 6 * 4] {
    let words = [
        MOVZ_X1_EVENT_SIGNALED,
        MOVZ_X2_ZERO,
        SVC_OBJECT_SIGNAL,
        MOVZ_X0_ZERO,
        SVC_THREAD_EXIT,
        B_LOOP,
    ];
    let mut bytes = [0u8; 6 * 4];
    for (i, w) in words.iter().enumerate() {
        bytes[i * 4..(i + 1) * 4].copy_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn aligned(va: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(va).expect("user VA must be 4K aligned")
}

fn userspace_spawn_user_process_runs_to_exit() {
    let event = Event::new();
    let payload = build_payload();
    let segment = UserSegment {
        va_base: aligned(USER_PAYLOAD_VA),
        mapped_size: PAGE_SIZE,
        init_bytes: &payload,
        perms: MemFlags::user_rx(),
    };
    let image = UserImage {
        segments: core::slice::from_ref(&segment),
        entry: VirtualAddress::new(USER_PAYLOAD_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP),
        user_stack_size: USER_STACK_SIZE,
    };

    let handle = Handle::new(KObject::Event(event.clone()), Rights::SIGNAL);
    let launch = UserProcessLaunch::new()
        .initial_handles(vec![handle])
        .bootstrap_handle(0);
    let info = super::user_process_launcher()
        .spawn_user_process_with_launch(
            "user-via-scheduler",
            &image,
            Priority::highest(),
            2,
            launch,
        )
        .expect("spawn_user_process must succeed");
    qemu_test_harness::kassert_eq!(info.initial_handle_ids.len(), 1);

    let scheduler = super::scheduler().clone();
    let mut spins = 0u64;
    while event.peek() & EVENT_SIGNALED == 0 {
        scheduler.sleep_ms(10);
        spins += 1;
        qemu_test_harness::kassert!(spins < 500);
    }

    let _ = spins;
}

register_test!(
    USERSPACE_SPAWN_USER_PROCESS_RUNS_TO_EXIT,
    "userspace_spawn_user_process_runs_to_exit",
    userspace_spawn_user_process_runs_to_exit
);

// vm_allocate + vm_remap E2E через user-payload.
//
// Payload (см. `build_vm_payload`) вызывает:
//   1. `svc #MemoryAllocate` (size=4096, flags=ReadWrite) -> x0 = выданный VA.
//   2. Сохраняет VA в x19 и записывает байт в `[x19]` - доказательство, что
//      страница реально writable (mmu активен в user-AS, mapping создан).
//   3. `svc #MemoryRemap` (va=x19, size=4096, flags=ReadOnly).
//   4. `svc #ObjectSignal` на bootstrap Event.
//   5. `svc #ThreadExit`.
//
// Тест ждёт сигнал. Сам факт того, что сигнал пришёл,
// означает: оба syscall'а отработали без аварийного завершения payload'а.

/// Сборка байт-кода для теста vm_allocate+vm_remap. Все инструкции -
/// little-endian, 4 байта каждая.
fn build_vm_payload() -> [u8; 18 * 4] {
    const SVC_MEMORY_ALLOCATE: u32 = svc(SyscallOp::MemoryAllocate);
    const SVC_MEMORY_REMAP: u32 = svc(SyscallOp::MemoryRemap);

    // mov  x21, x0                  ; save bootstrap Event handle
    const MOV_X21_X0: u32 = 0xAA00_03F5;
    // movz x0, #0x1000              ; size = 4096
    const MOVZ_X0_PAGE: u32 = 0xD282_0000;
    // movz x1, #0                   ; flags = ReadWrite
    const MOVZ_X1_ZERO: u32 = 0xD280_0001;
    // mov  x19, x0                  ; save VA in callee-preserved reg
    const MOV_X19_X0: u32 = 0xAA00_03F3;
    // movz w20, #0x42               ; sentinel byte
    const MOVZ_W20_SENTINEL: u32 = 0x5280_0854;
    // strb w20, [x19]               ; убедиться, что страница writable
    const STRB_W20_X19: u32 = 0x3900_0274;
    // mov  x0, x19                  ; remap-arg0 = VA
    const MOV_X0_X19: u32 = 0xAA13_03E0;
    // movz x1, #0x1000              ; remap-arg1 = size
    const MOVZ_X1_PAGE: u32 = 0xD282_0001;
    // movz x2, #1                   ; remap-arg2 = ReadOnly
    const MOVZ_X2_ONE: u32 = 0xD280_0022;
    // mov  x0, x21                  ; signal-arg0 = Event handle
    const MOV_X0_X21: u32 = 0xAA15_03E0;

    let words: [u32; 18] = [
        MOV_X21_X0,
        MOVZ_X0_PAGE,
        MOVZ_X1_ZERO,
        SVC_MEMORY_ALLOCATE,
        MOV_X19_X0,
        MOVZ_W20_SENTINEL,
        STRB_W20_X19,
        MOV_X0_X19,
        MOVZ_X1_PAGE,
        MOVZ_X2_ONE,
        SVC_MEMORY_REMAP,
        MOV_X0_X21,
        MOVZ_X1_EVENT_SIGNALED,
        MOVZ_X2_ZERO,
        SVC_OBJECT_SIGNAL,
        MOVZ_X0_ZERO,
        SVC_THREAD_EXIT,
        B_LOOP,
    ];

    let mut bytes = [0u8; 18 * 4];
    for (i, w) in words.iter().enumerate() {
        bytes[i * 4..(i + 1) * 4].copy_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn userspace_vm_allocate_and_remap() {
    let event = Event::new();
    let payload = build_vm_payload();
    let segment = UserSegment {
        va_base: aligned(USER_PAYLOAD_VA),
        mapped_size: PAGE_SIZE,
        init_bytes: &payload,
        perms: MemFlags::user_rx(),
    };
    let image = UserImage {
        segments: core::slice::from_ref(&segment),
        entry: VirtualAddress::new(USER_PAYLOAD_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP),
        user_stack_size: USER_STACK_SIZE,
    };

    let handle = Handle::new(KObject::Event(event.clone()), Rights::SIGNAL);
    let launch = UserProcessLaunch::new()
        .initial_handles(vec![handle])
        .bootstrap_handle(0);
    let info = super::user_process_launcher()
        .spawn_user_process_with_launch("user-vm-allocate", &image, Priority::highest(), 2, launch)
        .expect("spawn_user_process must succeed");
    qemu_test_harness::kassert_eq!(info.initial_handle_ids.len(), 1);

    let scheduler = super::scheduler().clone();
    let mut spins = 0u64;
    while event.peek() & EVENT_SIGNALED == 0 {
        scheduler.sleep_ms(10);
        spins += 1;
        qemu_test_harness::kassert!(spins < 500);
    }

    let _ = spins;
}

register_test!(
    USERSPACE_VM_ALLOCATE_AND_REMAP,
    "userspace_vm_allocate_and_remap",
    userspace_vm_allocate_and_remap
);
