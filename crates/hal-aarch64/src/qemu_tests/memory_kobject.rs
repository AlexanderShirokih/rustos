//! E2E проверка `KObject::Memory` через `MemoryMap` + `MemoryRegionInspect`.

use alloc::{sync::Arc, vec};
use core::num::NonZeroUsize;

use kernelspace::syscall_bridge;
use kobject::{EVENT_SIGNALED, Event, Handle, KObject, Rights};
use memory::{
    AccessMask, MemFlags, MemoryRegion,
    aligned::Aligned,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use scheduler::{Priority, SchedulerServiceExt, UserProcessLaunch};
use syscall::SyscallOp;
use test_harness_qemu::register_test;
use userspace::{UserImage, UserSegment};

use super::user_payload::{B_LOOP, Builder, Reg, b_ne, cmp_x, mov_x, str_x, svc_op};

const PAGE_SIZE: usize = PageAlignedVirtualAddress::ALIGNMENT;
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
const USER_STACK_SIZE: usize = PAGE_SIZE;

const PATTERN_LO: u16 = 0xBABE;
const PATTERN_MID_LO: u16 = 0xCAFE;
const PATTERN_MID_HI: u16 = 0xBEEF;
const PATTERN_HI: u16 = 0xDEAD;
const PATTERN: u64 = ((PATTERN_HI as u64) << 48)
    | ((PATTERN_MID_HI as u64) << 32)
    | ((PATTERN_MID_LO as u64) << 16)
    | (PATTERN_LO as u64);

/// HandleId-ы для свежевышедшей таблицы детерминированы:
/// первый insert — slot=0 generation=1 → raw = 0x0001_0000.
/// Второй — slot=1 generation=1 → raw = 0x0001_0001.
const HANDLE_EVENT_RAW: u32 = 0x0001_0001;

const EXPECTED_SIZE: u64 = PAGE_SIZE as u64;
const EXPECTED_KIND_TAG: u8 = 1; // Virtual
const EXPECTED_ACCESS: u8 = AccessMask::RW.bits();
const EXPECTED_INSPECT_SECONDARY: u64 =
    ((EXPECTED_KIND_TAG as u64) << 16) | (EXPECTED_ACCESS as u64);

/// Сборка байт-кода теста.
///
/// payload (на входе x0 = region_handle):
/// ```text
///   mov  x21, x0                  ; x21 = region_handle
///   mov  x0,  x21                 ; arg0 = handle
///   movz x1,  #0x1000             ; arg1 = size
///   movz x2,  #0                  ; arg2 = flags = RW
///   svc  #MemoryMap               ; x0 = va
///   mov  x19, x0                  ; x19 = va
///   movz x20, #PATTERN_LO
///   movk x20, #PATTERN_MID_LO, lsl 16
///   movk x20, #PATTERN_MID_HI, lsl 32
///   movk x20, #PATTERN_HI, lsl 48
///   str  x20, [x19]               ; *va = pattern
///
///   mov  x0,  x21
///   svc  #MemoryRegionInspect     ; x0 = size, x1 = kind|access (secondary)
///   movz x22, #(EXPECTED_SIZE)
///   cmp  x0,  x22
///   b.ne fail
///   movz x23, #EXPECTED_INSPECT_SECONDARY
///   cmp  x1,  x23
///   b.ne fail
///
///   movz x0,  #event_handle (HANDLE_EVENT_RAW_*)
///   movz x1,  #1            ; EVENT_SIGNALED
///   movz x2,  #0
///   svc  #ObjectSignal
/// fail: (skipped on success path; just falls through to ThreadExit anyway)
///   movz x0,  #0
///   svc  #ThreadExit
///   b .                            ; fallback
/// ```
fn build_payload() -> [u8; 28 * 4] {
    let mut payload = Builder::<28>::new(B_LOOP);

    // mov x21, x0
    payload.push(mov_x(Reg::X21, Reg::X0));

    // MemoryMap(handle=x21, size=4096, flags=0)
    payload.push(mov_x(Reg::X0, Reg::X21));
    payload.mov_u16(Reg::X1, 0x1000);
    payload.mov_u16(Reg::X2, 0);
    payload.push(svc_op(SyscallOp::MemoryMap));

    // mov x19, x0  ; va
    payload.push(mov_x(Reg::X19, Reg::X0));

    // load pattern in x20
    payload.mov_u64_fixed(Reg::X20, PATTERN);

    // str x20, [x19]
    payload.push(str_x(Reg::X20, Reg::X19));

    // MemoryRegionInspect(handle=x21)
    payload.push(mov_x(Reg::X0, Reg::X21));
    payload.push(svc_op(SyscallOp::MemoryRegionInspect));

    // x22 = EXPECTED_SIZE; cmp x0, x22; b.ne to thread_exit (fail path skips signal)
    payload.mov_u16(Reg::X22, EXPECTED_SIZE as u16);
    payload.push(cmp_x(Reg::X0, Reg::X22));
    // skip 7 words (через signal) и попасть на ThreadExit
    payload.push(b_ne(8));

    // x23 = EXPECTED_INSPECT_SECONDARY; cmp x1, x23; b.ne to thread_exit
    payload.mov_u32_fixed(Reg::X23, EXPECTED_INSPECT_SECONDARY as u32);
    payload.push(cmp_x(Reg::X1, Reg::X23));
    // skip 5 words до ThreadExit
    payload.push(b_ne(5));

    // ObjectSignal(event_handle, EVENT_SIGNALED, 0)
    payload.mov_u32_fixed(Reg::X0, HANDLE_EVENT_RAW);
    payload.mov_u16(Reg::X1, EVENT_SIGNALED as u16);
    payload.mov_u16(Reg::X2, 0);
    payload.push(svc_op(SyscallOp::ObjectSignal));

    // ThreadExit(0)
    payload.mov_u16(Reg::X0, 0);
    payload.push(svc_op(SyscallOp::ThreadExit));
    payload.push(B_LOOP);

    payload.assert_full();
    payload.into_bytes()
}

fn aligned(va: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(va).expect("user VA must be 4K aligned")
}

fn userspace_memory_kobject_map_and_inspect() {
    let fa = syscall_bridge::frame_allocator().expect("FrameAllocator must be installed");
    let region = MemoryRegion::create_virtual(fa, NonZeroUsize::new(1).unwrap(), AccessMask::RW)
        .expect("region create must succeed");
    let region_arc = Arc::new(region);

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

    let region_ko = KObject::Memory(region_arc.clone());
    let region_handle = Handle::new(
        region_ko,
        Rights::defaults_for(&KObject::Memory(region_arc)),
    );
    let event_handle = Handle::new(KObject::Event(event.clone()), Rights::SIGNAL);
    let launch = UserProcessLaunch::new()
        .initial_handles(vec![region_handle, event_handle])
        .bootstrap_handle(0);
    let info = kernelspace::qemu_tests::user_process_launcher()
        .spawn_user_process_with_launch(
            "user-memory-kobject",
            &image,
            Priority::highest(),
            2,
            launch,
        )
        .expect("spawn_user_process must succeed");
    test_harness_qemu::kassert_eq!(info.initial_handle_ids.len(), 2);
    // Подтверждаем детерминированные raw-значения, на которых построен payload.
    test_harness_qemu::kassert_eq!(info.initial_handle_ids[0].raw().get(), 0x0001_0000);
    test_harness_qemu::kassert_eq!(info.initial_handle_ids[1].raw().get(), 0x0001_0001);

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
    USERSPACE_MEMORY_KOBJECT_MAP_AND_INSPECT,
    "userspace_memory_kobject_map_and_inspect",
    userspace_memory_kobject_map_and_inspect
);
