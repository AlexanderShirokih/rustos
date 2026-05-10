//! E2E-проверка lifecycle Process/Thread KO через user-syscall'ы.
//!
//! Покрытые пути:
//!   1. `ProcessSelf` / `ThreadSelf` из EL0 возвращают ненулевые handle'ы.
//!   2. `ThreadExit(N)` поднимает `THREAD_TERMINATED` на `Arc<ThreadObject>`
//!      стартового потока и `PROCESS_TERMINATED` на `Arc<ProcessObject>`
//!      процесса, наблюдаемые kernel-side через `UserProcessLaunchInfo`.
//!   3. exit_code публикуется до сигнала и читается обоими `*_object`.

use alloc::vec;

use kobject::{
    EVENT_SIGNALED, Event, Handle, KObject, PROCESS_TERMINATED, Rights, THREAD_TERMINATED,
};
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
const PAYLOAD_EXIT_CODE: i32 = 42;

const fn svc(op: SyscallOp) -> u32 {
    0xD400_0001 | ((op as u32) << 5)
}

const SVC_PROCESS_SELF: u32 = svc(SyscallOp::ProcessSelf);
const SVC_THREAD_SELF: u32 = svc(SyscallOp::ThreadSelf);
const SVC_OBJECT_SIGNAL: u32 = svc(SyscallOp::ObjectSignal);
const SVC_THREAD_EXIT: u32 = svc(SyscallOp::ThreadExit);
const SVC_HANDLE_CLOSE: u32 = svc(SyscallOp::HandleClose);

// `mov xN, xM` = `orr xN, xzr, xM`.
const MOV_X21_X0: u32 = 0xAA00_03F5;

// `movz xN, #imm`: `D2_80_<imm16<<5|N>`.
const MOVZ_X1_EVENT_SIGNALED: u32 = 0xD280_0021;
const MOVZ_X2_ZERO: u32 = 0xD280_0002;
// `movz x0, #PAYLOAD_EXIT_CODE` (=42): D2_80_<imm<<5|0>.
const MOVZ_X0_EXIT_CODE: u32 = 0xD280_0000 | ((PAYLOAD_EXIT_CODE as u32) << 5);

// `tbnz xN, #63, +offset`: проверка отрицательного значения (старший бит).
// encoding: `B7_<imm14<<5|rt>` с `b40 = 1` для bit 63 -> `B7_F8_<offset<<5|rt>`.
// CBZ x0, +offset (если x0 == 0 то фейл): `B400_<imm19<<5|0>`.
const B_LOOP: u32 = 0x1400_0000;

const NUM_INSTRUCTIONS: usize = 16;

fn build_payload() -> [u8; NUM_INSTRUCTIONS * 4] {
    // CBZ x0, +N — переход к B_LOOP, если handle нулевой (т.е. ошибка).
    // Возможные fail-точки: индексы 2 (после ProcessSelf), 4 (после ThreadSelf).
    // Encoding `B400_<imm19<<5|rt>`. imm19 — смещение в инструкциях.
    const fn cbz_x0(off: u32) -> u32 {
        0xB400_0000 | ((off & 0x7FFFF) << 5)
    }
    // TBNZ x0, #63, +offset — переход, если старший бит установлен (sign-bit
    // отрицательного `i64`-результата ABI). encoding:
    // `B7_<b5<<31 | b40<<19 | imm14<<5 | rt>`. b5=1, b40=1<<3+1=11111,
    // целое: 0xB7F8_0000 | ((off & 0x3FFF) << 5).
    const fn tbnz_x0_sign(off: u32) -> u32 {
        0xB7F8_0000 | ((off & 0x3FFF) << 5)
    }

    // Layout (16 инструкций, индексы):
    //  0: mov x21, x0           ; сохранить bootstrap event handle
    //  1: svc #ProcessSelf      ; x0 = handle / -err
    //  2: tbnz x0, #63, +13     ; -err -> [15] B_LOOP
    //  3: cbz  x0, +12          ; handle == 0 -> [15] B_LOOP
    //  4: svc #HandleClose      ; закрыть process self-handle (x0)
    //  5: svc #ThreadSelf       ; x0 = handle / -err
    //  6: tbnz x0, #63, +9      ; -err -> [15] B_LOOP
    //  7: cbz  x0, +8           ; handle == 0 -> [15] B_LOOP
    //  8: svc #HandleClose      ; закрыть thread self-handle
    //  9: mov x0, x21           ; bootstrap event handle
    // 10: movz x1, #EVENT_SIGNALED
    // 11: movz x2, #0
    // 12: svc #ObjectSignal
    // 13: movz x0, #PAYLOAD_EXIT_CODE
    // 14: svc #ThreadExit       ; не возвращается
    // 15: b .                   ; fallback, ловится timeout-ом
    const MOV_X0_X21: u32 = 0xAA15_03E0;

    let words: [u32; NUM_INSTRUCTIONS] = [
        MOV_X21_X0,
        SVC_PROCESS_SELF,
        tbnz_x0_sign(13),
        cbz_x0(12),
        SVC_HANDLE_CLOSE,
        SVC_THREAD_SELF,
        tbnz_x0_sign(9),
        cbz_x0(8),
        SVC_HANDLE_CLOSE,
        MOV_X0_X21,
        MOVZ_X1_EVENT_SIGNALED,
        MOVZ_X2_ZERO,
        SVC_OBJECT_SIGNAL,
        MOVZ_X0_EXIT_CODE,
        SVC_THREAD_EXIT,
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

fn process_lifecycle_self_handles_and_exit_code() {
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
        .spawn_user_process_with_launch("process-lifecycle", &image, Priority::highest(), 2, launch)
        .expect("spawn_user_process must succeed");

    let process_object = info.process_object.clone();
    let thread_object = info.thread_object.clone();

    let scheduler = super::scheduler().clone();
    let mut spins = 0u64;
    while event.peek() & EVENT_SIGNALED == 0 {
        scheduler.sleep_ms(10);
        spins += 1;
        test_harness_qemu::kassert!(spins < 500);
    }

    spins = 0;
    while thread_object.peek() & THREAD_TERMINATED == 0 {
        scheduler.sleep_ms(10);
        spins += 1;
        test_harness_qemu::kassert!(spins < 500);
    }
    test_harness_qemu::kassert_eq!(thread_object.exit_code(), PAYLOAD_EXIT_CODE);

    spins = 0;
    while process_object.peek() & PROCESS_TERMINATED == 0 {
        scheduler.sleep_ms(10);
        spins += 1;
        test_harness_qemu::kassert!(spins < 500);
    }
    test_harness_qemu::kassert_eq!(process_object.exit_code(), PAYLOAD_EXIT_CODE);
}

register_test!(
    PROCESS_LIFECYCLE_SELF_HANDLES_AND_EXIT_CODE,
    "process_lifecycle_self_handles_and_exit_code",
    process_lifecycle_self_handles_and_exit_code
);
