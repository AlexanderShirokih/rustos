//! E2E проверка `SchedulerService::spawn_user_process`.
//!
//! Тест передаёт payload'у bootstrap-handle на `Event`, ждёт сигнал от
//! `ObjectSignal`, затем payload делает `ThreadExit`.

use alloc::vec;

use kernel_tests::kernel_test;
use kobject::{EVENT_SIGNALED, Event, Handle, KObject, Rights};
use memory::{
    MemFlags,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use process::{UserImage, UserSegment};
use scheduler::{Priority, SchedulerServiceExt, UserProcessLaunch};
use syscall::SyscallOp;

use super::user_payload::{
    B_LOOP, Reg, b_ne, cmp_x, mov_x, movz_w, movz_x, strb_w, svc_op, words_to_bytes,
};

const PAGE_SIZE: usize = 4096;
/// Lower-half VA для payload - чистый user-AS, никаких пересечений.
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
/// Минимальный footprint: 1 страница стека достаточна, payload не пишет в стек.
const USER_STACK_SIZE: usize = PAGE_SIZE;

/// Сборка байт-кода: `ObjectSignal(x0, EVENT_SIGNALED, 0); ThreadExit(0); b .`.
/// `b .` - fallback на случай возврата (не должен исполниться).
fn build_payload() -> [u8; 6 * 4] {
    let words = [
        movz_x(Reg::X1, EVENT_SIGNALED as u16, 0),
        movz_x(Reg::X2, 0, 0),
        svc_op(SyscallOp::ObjectSignal),
        movz_x(Reg::X0, 0, 0),
        svc_op(SyscallOp::ThreadExit),
        B_LOOP,
    ];
    words_to_bytes(words)
}

fn aligned(va: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(va).expect("user VA must be 4K aligned")
}

#[kernel_test]
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
    let info = kernelspace::kernel_tests::user_process_launcher()
        .spawn_user_process_with_launch(
            "user-via-scheduler",
            &image,
            Priority::highest(),
            2,
            launch,
        )
        .expect("spawn_user_process must succeed");
    kernel_tests::kassert_eq!(info.initial_handle_ids.len(), 1);

    let scheduler = kernelspace::kernel_tests::scheduler().clone();
    let mut spins = 0u64;
    while event.peek() & EVENT_SIGNALED == 0 {
        scheduler.sleep_ms(10);
        spins += 1;
        kernel_tests::kassert!(spins < 500);
    }

    let _ = spins;
}

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
    let words = [
        mov_x(Reg::X21, Reg::X0),
        movz_x(Reg::X0, 0x1000, 0),
        movz_x(Reg::X1, 0, 0),
        svc_op(SyscallOp::MemoryAllocate),
        mov_x(Reg::X19, Reg::X0),
        movz_w(Reg::X20, 0x42),
        strb_w(Reg::X20, Reg::X19),
        mov_x(Reg::X0, Reg::X19),
        movz_x(Reg::X1, 0x1000, 0),
        movz_x(Reg::X2, 1, 0),
        svc_op(SyscallOp::MemoryRemap),
        mov_x(Reg::X0, Reg::X21),
        movz_x(Reg::X1, EVENT_SIGNALED as u16, 0),
        movz_x(Reg::X2, 0, 0),
        svc_op(SyscallOp::ObjectSignal),
        movz_x(Reg::X0, 0, 0),
        svc_op(SyscallOp::ThreadExit),
        B_LOOP,
    ];
    words_to_bytes(words)
}

#[kernel_test]
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
    let info = kernelspace::kernel_tests::user_process_launcher()
        .spawn_user_process_with_launch("user-vm-allocate", &image, Priority::highest(), 2, launch)
        .expect("spawn_user_process must succeed");
    kernel_tests::kassert_eq!(info.initial_handle_ids.len(), 1);

    let scheduler = kernelspace::kernel_tests::scheduler().clone();
    let mut spins = 0u64;
    while event.peek() & EVENT_SIGNALED == 0 {
        scheduler.sleep_ms(10);
        spins += 1;
        kernel_tests::kassert!(spins < 500);
    }

    let _ = spins;
}

// vm_allocate + vm_free + vm_allocate E2E (доказательство реюза VA).
//
// Сам факт прихода сигнала доказывает: vm_free вернул регион в free-list
// и следующий vm_allocate выдал тот же самый VA (coalesce + first-fit).

fn build_vm_free_payload() -> [u8; 22 * 4] {
    let words = [
        mov_x(Reg::X21, Reg::X0),
        movz_x(Reg::X0, 0x1000, 0),
        movz_x(Reg::X1, 0, 0),
        svc_op(SyscallOp::MemoryAllocate),
        mov_x(Reg::X19, Reg::X0),
        movz_w(Reg::X20, 0x42),
        strb_w(Reg::X20, Reg::X19),
        mov_x(Reg::X0, Reg::X19),
        movz_x(Reg::X1, 0x1000, 0),
        svc_op(SyscallOp::MemoryFree),
        movz_x(Reg::X0, 0x1000, 0),
        movz_x(Reg::X1, 0, 0),
        svc_op(SyscallOp::MemoryAllocate),
        cmp_x(Reg::X0, Reg::X19),
        b_ne(7),
        mov_x(Reg::X0, Reg::X21),
        movz_x(Reg::X1, EVENT_SIGNALED as u16, 0),
        movz_x(Reg::X2, 0, 0),
        svc_op(SyscallOp::ObjectSignal),
        movz_x(Reg::X0, 0, 0),
        svc_op(SyscallOp::ThreadExit),
        B_LOOP,
    ];
    words_to_bytes(words)
}

#[kernel_test]
fn userspace_vm_allocate_free_reuse_va() {
    let event = Event::new();
    let payload = build_vm_free_payload();
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
    let info = kernelspace::kernel_tests::user_process_launcher()
        .spawn_user_process_with_launch(
            "user-vm-free-reuse",
            &image,
            Priority::highest(),
            2,
            launch,
        )
        .expect("spawn_user_process must succeed");
    kernel_tests::kassert_eq!(info.initial_handle_ids.len(), 1);

    let scheduler = kernelspace::kernel_tests::scheduler().clone();
    let mut spins = 0u64;
    while event.peek() & EVENT_SIGNALED == 0 {
        scheduler.sleep_ms(10);
        spins += 1;
        kernel_tests::kassert!(spins < 500);
    }

    let _ = spins;
}
