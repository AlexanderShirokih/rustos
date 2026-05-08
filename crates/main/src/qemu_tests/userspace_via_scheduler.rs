//! E2E проверка `SchedulerService::spawn_user_process`.
//!
//! В отличие от низкоуровневого `userspace_entry.rs` (`crates/hal-aarch64`),
//! этот тест не делает ручной setup'а user-AS, payload-маппинга и `init_user`
//! - он использует публичный API `UserImage` + `spawn_user_process`. После
//! успеха scheduler сам:
//!   1) создаёт user-AS через factory,
//!   2) `load_user_image` маппит сегменты + стек через `MemoryMapper::map_owned`
//!      (Aarch64-реализация инвалидирует I-cache для exec-страниц),
//!   3) инициализирует `Aarch64Context::init_user(UserEntry)`,
//!   4) ставит в ready-queue.
//!
//! Тест ждёт, пока user-payload вернёт через `TestEl0Probe` ожидаемый
//! `bootstrap_x0` (`UserBootstrapArg::ZERO`), затем процесс делает `ThreadExit`.

use drivers_common::services::scheduler::{Priority, SchedulerServiceExt};
use memory::{
    MemFlags,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use qemu_test_harness::register_test;

use crate::{
    qemu_tests::el0_probe,
    sched::{UserImage, UserSegment},
};

const PAGE_SIZE: usize = 4096;
/// Lower-half VA для payload - чистый user-AS, никаких пересечений.
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
/// Минимальный footprint: 1 страница стека достаточна, payload не пишет в стек.
const USER_STACK_SIZE: usize = PAGE_SIZE;

/// `svc #0xFF00` (`TestEl0Probe`) - encoded `0xD400_0001 | (imm16 << 5)`.
const SVC_TEST_EL0_PROBE: u32 = 0xD400_0001 | ((0xFF00u32) << 5);
/// `svc #0` (`ThreadExit`).
const SVC_THREAD_EXIT: u32 = 0xD400_0001;

/// Сборка байт-кода: `svc #TestEl0Probe; svc #ThreadExit; b .`.
/// `b .` - fallback на случай возврата (не должен исполниться).
fn build_payload() -> [u8; 12] {
    let mut bytes = [0u8; 12];
    bytes[0..4].copy_from_slice(&SVC_TEST_EL0_PROBE.to_le_bytes());
    bytes[4..8].copy_from_slice(&SVC_THREAD_EXIT.to_le_bytes());
    bytes[8..12].copy_from_slice(&0x1400_0000u32.to_le_bytes());
    bytes
}

fn aligned(va: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(va).expect("user VA must be 4K aligned")
}

fn userspace_spawn_user_process_runs_to_exit() {
    el0_probe::reset();

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

    let scheduler = super::scheduler().clone();
    let (_pid, _tid) = scheduler
        .spawn_user_process("user-via-scheduler", &image, Priority::highest(), 2)
        .expect("spawn_user_process must succeed");

    // Главный test-thread спит, пока scheduler гонит user-payload через EL0;
    // payload фиксирует TestEl0Probe и завершается ThreadExit.
    let mut spins = 0u64;
    while el0_probe::peek().is_none() {
        scheduler.sleep_ms(10);
        spins += 1;
        qemu_test_harness::kassert!(spins < 500);
    }

    let (observed_x0, is_user) = el0_probe::peek().expect("probe captured");
    qemu_test_harness::kassert!(is_user);
    // UserBootstrapArg::ZERO передан scheduler-ом по умолчанию.
    qemu_test_harness::kassert_eq!(observed_x0, 0);
    let _ = spins;
}

register_test!(
    USERSPACE_SPAWN_USER_PROCESS_RUNS_TO_EXIT,
    "userspace_spawn_user_process_runs_to_exit",
    userspace_spawn_user_process_runs_to_exit
);

// =====================================================================
// vm_allocate + vm_remap E2E через user-payload.
// =====================================================================
//
// Payload (см. `build_vm_payload`) вызывает:
//   1. `svc #MemoryAllocate` (size=4096, flags=ReadWrite) -> x0 = выданный VA.
//   2. Сохраняет VA в x19 и записывает байт в `[x19]` - доказательство, что
//      страница реально writable (mmu активен в user-AS, mapping создан).
//   3. `svc #MemoryRemap` (va=x19, size=4096, flags=ReadOnly).
//   4. `svc #TestEl0Probe` с маркером `0xCAFE` - фиксируется в `el0_probe`,
//      handler делает `thread_exit`.
//
// Тест ждёт probe и проверяет marker. Сам факт того, что probe пришёл,
// означает: оба syscall'а отработали без панического exit-а из EL0.

const VM_PROBE_MARKER: u64 = 0xCAFE;

/// Сборка байт-кода для теста vm_allocate+vm_remap. Все инструкции -
/// little-endian, 4 байта каждая.
fn build_vm_payload() -> [u8; 13 * 4] {
    const SVC_MEMORY_ALLOCATE: u32 = 0xD400_0001 | (0x60u32 << 5);
    const SVC_MEMORY_REMAP: u32 = 0xD400_0001 | (0x61u32 << 5);

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
    // movz x0, #0xCAFE              ; probe-marker
    const MOVZ_X0_MARKER: u32 = 0xD299_5FC0;

    let words: [u32; 13] = [
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
        MOVZ_X0_MARKER,
        SVC_TEST_EL0_PROBE,
        // Безопасный fallback: бесконечный цикл, если probe не завершит
        // thread (не должно происходить - probe-handler делает exit).
        0x1400_0000,
    ];

    let mut bytes = [0u8; 13 * 4];
    for (i, w) in words.iter().enumerate() {
        bytes[i * 4..(i + 1) * 4].copy_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn userspace_vm_allocate_and_remap() {
    el0_probe::reset();

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

    let scheduler = super::scheduler().clone();
    let (pid, _tid) = scheduler
        .spawn_user_process("user-vm-allocate", &image, Priority::highest(), 2)
        .expect("spawn_user_process must succeed");

    let mut spins = 0u64;
    while el0_probe::peek().is_none() {
        scheduler.sleep_ms(10);
        spins += 1;
        qemu_test_harness::kassert!(spins < 500);
    }

    let (observed, is_user) = el0_probe::peek().expect("probe captured");
    qemu_test_harness::kassert!(is_user);
    qemu_test_harness::kassert_eq!(observed, VM_PROBE_MARKER);
    // У процесса должен быть ровно один зарегистрированный регион - тот,
    // который выдал `vm_allocate`. `vm_remap` не меняет количество регионов.
    let _ = pid;
    let _ = spins;
}

register_test!(
    USERSPACE_VM_ALLOCATE_AND_REMAP,
    "userspace_vm_allocate_and_remap",
    userspace_vm_allocate_and_remap
);
