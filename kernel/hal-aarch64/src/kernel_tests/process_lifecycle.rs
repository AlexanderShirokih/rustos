//! Kernel-side проверка завершения user-процесса: `ThreadExit(N)` из EL0
//! помечает завершённым `Arc<ThreadObject>` стартового потока и
//! `Arc<ProcessObject>` из `UserProcessLaunchInfo`; exit_code публикуется
//! до пометки и читается обоими `*_object`.

use kernel_tests::kernel_test;
use memory::{
    MemFlags,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use process::{UserImage, UserSegment};
use scheduler::{Priority, SchedulerServiceExt, UserProcessLaunch};
use syscall::SyscallOp;

use super::user_payload::{B_LOOP, Reg, movz_x, svc_op, words_to_bytes};

const PAGE_SIZE: usize = 4096;
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
const USER_STACK_SIZE: usize = PAGE_SIZE;
const PAYLOAD_EXIT_CODE: i32 = 42;

/// Payload: `ThreadExit(PAYLOAD_EXIT_CODE)`; `b .` - fallback на случай
/// возврата (не должен исполниться).
fn build_payload() -> [u8; 3 * 4] {
    let words = [
        movz_x(Reg::X0, PAYLOAD_EXIT_CODE as u16, 0),
        svc_op(SyscallOp::ThreadExit),
        B_LOOP,
    ];
    words_to_bytes(words)
}

fn aligned(va: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(va).expect("user VA must be 4K aligned")
}

#[kernel_test]
fn process_lifecycle_exit_code_and_termination_signals() {
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

    let info = kernelspace::kernel_tests::user_process_launcher()
        .spawn_user_process_with_launch(
            "process-lifecycle",
            &image,
            Priority::highest(),
            2,
            UserProcessLaunch::new(),
        )
        .expect("spawn_user_process must succeed");

    let process_object = info.process_object.clone();
    let thread_object = info.thread_object.clone();

    let scheduler = kernelspace::kernel_tests::scheduler().clone();
    let mut spins = 0u64;
    while !thread_object.terminated() {
        scheduler.sleep_ms(10);
        spins += 1;
        kernel_tests::kassert!(spins < 500);
    }
    kernel_tests::kassert_eq!(thread_object.exit_code(), PAYLOAD_EXIT_CODE);

    spins = 0;
    while !process_object.terminated() {
        scheduler.sleep_ms(10);
        spins += 1;
        kernel_tests::kassert!(spins < 500);
    }
    kernel_tests::kassert_eq!(process_object.exit_code(), PAYLOAD_EXIT_CODE);
}
