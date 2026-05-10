//! E2E проверка `MailboxCreate`/`MailboxQueue`/`MailboxWait`-syscall'ов
//! из user-mode.
//!
//! Payload (см. [`build_payload`]) через SVC: создаёт mailbox, выделяет
//! 4К-страницу, кладёт туда user-пакет с известным `key`, пушит через
//! `MailboxQueue`, читает обратно через `MailboxWait` (poll, без
//! таймаута) и проверяет, что `key` сохранился. Любая ошибка уводит в
//! `b .` - тест валится по timeout-у harness'а.

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
    B_LOOP, Reg, b_ne, cbnz_x, cmp_x, cmp_x_imm12, ldr_x, mov_x, movk_x, movz_x, str_x, svc_op,
    words_to_bytes,
};

const PAGE_SIZE: usize = 4096;
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
const USER_STACK_SIZE: usize = PAGE_SIZE;

const NUM_INSTRUCTIONS: usize = 34;

fn build_payload() -> [u8; NUM_INSTRUCTIONS * 4] {
    let words = [
        // [0] save bootstrap event handle.
        mov_x(Reg::X21, Reg::X0),
        // [1] MailboxCreate -> x0=mbox.
        svc_op(SyscallOp::MailboxCreate),
        // [2] x22 = mbox.
        mov_x(Reg::X22, Reg::X0),
        // [3..5] vm_allocate(0x1000, 0).
        movz_x(Reg::X0, 0x1000, 0),
        movz_x(Reg::X1, 0, 0),
        svc_op(SyscallOp::MemoryAllocate),
        // [6] x19 = buffer addr.
        mov_x(Reg::X19, Reg::X0),
        // [7..8] x20 = 0xCAFE_BABE (lo32 ключа). hi32 остаётся нулём -
        // страница свежевыделена, MemoryAllocate возвращает обнулённую.
        movz_x(Reg::X20, 0xBABE, 0),
        movk_x(Reg::X20, 0xCAFE, 1),
        // [9] записываем key в буфер. Остальные поля пакета
        // (kind=User=0, status=0, payload) остаются нулевыми и
        // проходят валидацию.
        str_x(Reg::X20, Reg::X19),
        // [10..13] MailboxQueue(mbox, buf, 32).
        mov_x(Reg::X0, Reg::X22),
        mov_x(Reg::X1, Reg::X19),
        movz_x(Reg::X2, 32, 0),
        svc_op(SyscallOp::MailboxQueue),
        // [14] queue != 0 -> fail (target = 33, offset = 19).
        cbnz_x(Reg::X0, 19),
        // [15..19] MailboxWait(mbox, 0, buf, 32).
        mov_x(Reg::X0, Reg::X22),
        movz_x(Reg::X1, 0, 0),
        mov_x(Reg::X2, Reg::X19),
        movz_x(Reg::X3, 32, 0),
        svc_op(SyscallOp::MailboxWait),
        // [20..21] x0 == 32 ? (target = 33, offset = 12).
        cmp_x_imm12(Reg::X0, 32),
        b_ne(12),
        // [22..26] прочитанный key совпадает с записанным
        // (target = 33, offset = 7).
        ldr_x(Reg::X0, Reg::X19),
        movz_x(Reg::X1, 0xBABE, 0),
        movk_x(Reg::X1, 0xCAFE, 1),
        cmp_x(Reg::X0, Reg::X1),
        b_ne(7),
        // [27..30] signal bootstrap event.
        mov_x(Reg::X0, Reg::X21),
        movz_x(Reg::X1, EVENT_SIGNALED as u16, 0),
        movz_x(Reg::X2, 0, 0),
        svc_op(SyscallOp::ObjectSignal),
        // [31..32] thread_exit(0).
        movz_x(Reg::X0, 0, 0),
        svc_op(SyscallOp::ThreadExit),
        // [33] fail / fallback - infinite loop, тест валится по timeout-у.
        B_LOOP,
    ];
    words_to_bytes(words)
}

fn aligned(va: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(va).expect("user VA must be 4K aligned")
}

fn mailbox_via_syscall_round_trip() {
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
            "mailbox-via-syscall",
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
    MAILBOX_VIA_SYSCALL_ROUND_TRIP,
    "mailbox_via_syscall_round_trip",
    mailbox_via_syscall_round_trip
);
