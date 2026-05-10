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

use super::user_payload::{B_LOOP, Reg, cbz_x, mov_x, movz_x, svc_op, tbnz_x, words_to_bytes};

const PAGE_SIZE: usize = 4096;
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
const USER_STACK_SIZE: usize = PAGE_SIZE;
const PAYLOAD_EXIT_CODE: i32 = 42;

const NUM_INSTRUCTIONS: usize = 16;

fn build_payload() -> [u8; NUM_INSTRUCTIONS * 4] {
    // CBZ x0, +N - переход к B_LOOP, если handle нулевой (т.е. ошибка).
    // Возможные fail-точки: индексы 2 (после ProcessSelf), 4 (после ThreadSelf).

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
    let words = [
        mov_x(Reg::X21, Reg::X0),
        svc_op(SyscallOp::ProcessSelf),
        tbnz_x(Reg::X0, 63, 13),
        cbz_x(Reg::X0, 12),
        svc_op(SyscallOp::HandleClose),
        svc_op(SyscallOp::ThreadSelf),
        tbnz_x(Reg::X0, 63, 9),
        cbz_x(Reg::X0, 8),
        svc_op(SyscallOp::HandleClose),
        mov_x(Reg::X0, Reg::X21),
        movz_x(Reg::X1, EVENT_SIGNALED as u16, 0),
        movz_x(Reg::X2, 0, 0),
        svc_op(SyscallOp::ObjectSignal),
        movz_x(Reg::X0, PAYLOAD_EXIT_CODE as u16, 0),
        svc_op(SyscallOp::ThreadExit),
        B_LOOP,
    ];
    words_to_bytes(words)
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
    let info = kernelspace::qemu_tests::user_process_launcher()
        .spawn_user_process_with_launch("process-lifecycle", &image, Priority::highest(), 2, launch)
        .expect("spawn_user_process must succeed");

    let process_object = info.process_object.clone();
    let thread_object = info.thread_object.clone();

    let scheduler = kernelspace::qemu_tests::scheduler().clone();
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
