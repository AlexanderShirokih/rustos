//! E2E проверка `ChannelWrite`/`ChannelRead`-syscall'ов из user-mode.
//!
//! Payload (см. [`build_payload`]) через SVC: `ChannelCreate`,
//! `MemoryAllocate(4K, RW)`, кладёт в буфер `0x42`,
//! `ChannelWrite(left, buf, 1, 0, 0)`, `ChannelRead(right, buf, 256, 0, 0)`
//! (возврат должен быть `1`: 1 байт payload, 0 handle'ов),
//! `ObjectSignal(bootstrap_event, EVENT_SIGNALED, 0)`, `ThreadExit(0)`.
//! Любая ошибка уводит в `b .` - тест валится по timeout-у.

use alloc::vec;

use kobject::{EVENT_SIGNALED, Event, Handle, KObject, Rights};
use memory::{
    MemFlags,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use scheduler::{Priority, SchedulerServiceExt, UserProcessLaunch};
use syscall::SyscallOp;
use test_harness_qemu::register_test;
use userspace::{UserImage, UserSegment};

const PAGE_SIZE: usize = 4096;
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
const USER_STACK_SIZE: usize = PAGE_SIZE;

const fn svc(op: SyscallOp) -> u32 {
    0xD400_0001 | ((op as u32) << 5)
}

const SVC_CHANNEL_CREATE: u32 = svc(SyscallOp::ChannelCreate);
const SVC_CHANNEL_WRITE: u32 = svc(SyscallOp::ChannelWrite);
const SVC_CHANNEL_READ: u32 = svc(SyscallOp::ChannelRead);
const SVC_MEMORY_ALLOCATE: u32 = svc(SyscallOp::MemoryAllocate);
const SVC_OBJECT_SIGNAL: u32 = svc(SyscallOp::ObjectSignal);
const SVC_THREAD_EXIT: u32 = svc(SyscallOp::ThreadExit);

// `mov xN, xM` = `orr xN, xzr, xM` -> `aa<m>03e<n>` где младшие 5 бит -
// rd, биты [20:16] - rm.
const MOV_X21_X0: u32 = 0xAA00_03F5;
const MOV_X22_X0: u32 = 0xAA00_03F6;
const MOV_X23_X1: u32 = 0xAA01_03F7;
const MOV_X19_X0: u32 = 0xAA00_03F3;
const MOV_X0_X22: u32 = 0xAA16_03E0;
const MOV_X1_X19: u32 = 0xAA13_03E1;
const MOV_X0_X23: u32 = 0xAA17_03E0;
const MOV_X0_X21: u32 = 0xAA15_03E0;

// `movz xN, #imm`: `D2_80_<imm16<<5|N>`.
const MOVZ_X0_PAGE: u32 = 0xD282_0000; // x0 = 0x1000
const MOVZ_X1_ZERO: u32 = 0xD280_0001; // x1 = 0
const MOVZ_X2_ONE: u32 = 0xD280_0022; // x2 = 1
const MOVZ_X2_256: u32 = 0xD280_2002; // x2 = 256
const MOVZ_X3_ZERO: u32 = 0xD280_0003;
const MOVZ_X4_ZERO: u32 = 0xD280_0004;
const MOVZ_X1_EVENT_SIGNALED: u32 = 0xD280_0021; // x1 = EVENT_SIGNALED (=1)
const MOVZ_X2_ZERO: u32 = 0xD280_0002;
const MOVZ_X0_ZERO: u32 = 0xD280_0000;

// `movz w20, #0x42`: `52_80_<0x42<<5|20>` = `52_80_08_54`.
const MOVZ_W20_SENTINEL: u32 = 0x5280_0854;
// `strb w20, [x19]`: store-byte unsigned offset 0.
const STRB_W20_X19: u32 = 0x3900_0274;
// `cmp x0, #1`: `subs xzr, x0, #1, lsl #0` -> `F100_041F`.
const CMP_X0_ONE: u32 = 0xF100_041F;
// `b .`: infinite loop.
const B_LOOP: u32 = 0x1400_0000;

const NUM_INSTRUCTIONS: usize = 32;

/// Сборка байт-кода: payload занимает 32 инструкции в одной 4К-странице.
fn build_payload() -> [u8; NUM_INSTRUCTIONS * 4] {
    // CBNZ x0, +15: индекс 16, target 31 -> offset 15 (в инструкциях).
    // Encoding `B5_00_<imm19<<5|rt>` = `B500_0000 | (15 << 5) | 0` = `B500_01E0`.
    const CBNZ_X0_FAIL: u32 = 0xB500_01E0;
    // B.NE +7: индекс 24, target 31, offset 7. `54_00_<imm19<<5|cond=0001>` =
    // `54_00_<7<<5|1>` = `5400_00E1`.
    const B_NE_FAIL: u32 = 0x5400_00E1;

    let words: [u32; NUM_INSTRUCTIONS] = [
        // [0] save bootstrap event handle.
        MOV_X21_X0,
        // [1] ChannelCreate -> x0=left, x1=right.
        SVC_CHANNEL_CREATE,
        // [2] x22 = left, [3] x23 = right.
        MOV_X22_X0,
        MOV_X23_X1,
        // [4..7] vm_allocate(0x1000, 0).
        MOVZ_X0_PAGE,
        MOVZ_X1_ZERO,
        SVC_MEMORY_ALLOCATE,
        MOV_X19_X0,
        // [8..9] sentinel byte to buffer.
        MOVZ_W20_SENTINEL,
        STRB_W20_X19,
        // [10..15] ChannelWrite(left, buf, 1, 0, 0).
        MOV_X0_X22,
        MOV_X1_X19,
        MOVZ_X2_ONE,
        MOVZ_X3_ZERO,
        MOVZ_X4_ZERO,
        SVC_CHANNEL_WRITE,
        // [16] write != 0 -> fail.
        CBNZ_X0_FAIL,
        // [17..22] ChannelRead(right, buf, 256, 0, 0).
        MOV_X0_X23,
        MOV_X1_X19,
        MOVZ_X2_256,
        MOVZ_X3_ZERO,
        MOVZ_X4_ZERO,
        SVC_CHANNEL_READ,
        // [23] x0 == 1 ? (1 байт payload, 0 handle'ов).
        CMP_X0_ONE,
        // [24] not equal -> fail.
        B_NE_FAIL,
        // [25..28] signal bootstrap event.
        MOV_X0_X21,
        MOVZ_X1_EVENT_SIGNALED,
        MOVZ_X2_ZERO,
        SVC_OBJECT_SIGNAL,
        // [29..30] thread_exit(0).
        MOVZ_X0_ZERO,
        SVC_THREAD_EXIT,
        // [31] fail / fallback - infinite loop, тест валится по timeout-у.
        B_LOOP,
    ];

    let mut bytes = [0u8; NUM_INSTRUCTIONS * 4];
    for (i, w) in words.iter().enumerate() {
        bytes[i * 4..(i + 1) * 4].copy_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn aligned(va: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(va).expect("user VA must be 4K aligned")
}

fn channel_via_syscall_round_trip() {
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
            "channel-via-syscall",
            &image,
            Priority::highest(),
            2,
            launch,
        )
        .expect("spawn_user_process must succeed");
    test_harness_qemu::kassert_eq!(info.initial_handle_ids.len(), 1);

    let scheduler = super::scheduler().clone();
    let mut spins = 0u64;
    while event.peek() & EVENT_SIGNALED == 0 {
        scheduler.sleep_ms(10);
        spins += 1;
        test_harness_qemu::kassert!(spins < 500);
    }

    let _ = spins;
}

register_test!(
    CHANNEL_VIA_SYSCALL_ROUND_TRIP,
    "channel_via_syscall_round_trip",
    channel_via_syscall_round_trip
);
