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

use super::user_payload::{
    B_LOOP, Reg, b_ne, cbnz_x, cmp_x_imm12, mov_x, movz_w, movz_x, strb_w, svc_op, words_to_bytes,
};

const PAGE_SIZE: usize = 4096;
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
const USER_STACK_SIZE: usize = PAGE_SIZE;

const NUM_INSTRUCTIONS: usize = 32;

/// Сборка байт-кода: payload занимает 32 инструкции в одной 4К-странице.
fn build_payload() -> [u8; NUM_INSTRUCTIONS * 4] {
    let words = [
        // [0] save bootstrap event handle.
        mov_x(Reg::X21, Reg::X0),
        // [1] ChannelCreate -> x0=left, x1=right.
        svc_op(SyscallOp::ChannelCreate),
        // [2] x22 = left, [3] x23 = right.
        mov_x(Reg::X22, Reg::X0),
        mov_x(Reg::X23, Reg::X1),
        // [4..7] vm_allocate(0x1000, 0).
        movz_x(Reg::X0, 0x1000, 0),
        movz_x(Reg::X1, 0, 0),
        svc_op(SyscallOp::MemoryAllocate),
        mov_x(Reg::X19, Reg::X0),
        // [8..9] sentinel byte to buffer.
        movz_w(Reg::X20, 0x42),
        strb_w(Reg::X20, Reg::X19),
        // [10..15] ChannelWrite(left, buf, 1, 0, 0).
        mov_x(Reg::X0, Reg::X22),
        mov_x(Reg::X1, Reg::X19),
        movz_x(Reg::X2, 1, 0),
        movz_x(Reg::X3, 0, 0),
        movz_x(Reg::X4, 0, 0),
        svc_op(SyscallOp::ChannelWrite),
        // [16] write != 0 -> fail.
        cbnz_x(Reg::X0, 15),
        // [17..22] ChannelRead(right, buf, 256, 0, 0).
        mov_x(Reg::X0, Reg::X23),
        mov_x(Reg::X1, Reg::X19),
        movz_x(Reg::X2, 256, 0),
        movz_x(Reg::X3, 0, 0),
        movz_x(Reg::X4, 0, 0),
        svc_op(SyscallOp::ChannelRead),
        // [23] x0 == 1 ? (1 байт payload, 0 handle'ов).
        cmp_x_imm12(Reg::X0, 1),
        // [24] not equal -> fail.
        b_ne(7),
        // [25..28] signal bootstrap event.
        mov_x(Reg::X0, Reg::X21),
        movz_x(Reg::X1, EVENT_SIGNALED as u16, 0),
        movz_x(Reg::X2, 0, 0),
        svc_op(SyscallOp::ObjectSignal),
        // [29..30] thread_exit(0).
        movz_x(Reg::X0, 0, 0),
        svc_op(SyscallOp::ThreadExit),
        // [31] fail / fallback - infinite loop, тест валится по timeout-у.
        B_LOOP,
    ];
    words_to_bytes(words)
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
    let info = kernelspace::qemu_tests::user_process_launcher()
        .spawn_user_process_with_launch(
            "channel-via-syscall",
            &image,
            Priority::highest(),
            2,
            launch,
        )
        .expect("spawn_user_process must succeed");
    test_harness_qemu::kassert_eq!(info.initial_handle_ids.len(), 1);

    let scheduler = kernelspace::qemu_tests::scheduler().clone();
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
