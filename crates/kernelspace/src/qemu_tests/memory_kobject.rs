//! E2E проверка `KObject::Memory` через `MemoryMap` + `MemoryRegionInspect`.

use alloc::{sync::Arc, vec};
use core::num::NonZeroUsize;

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

use crate::syscall_bridge;

const PAGE_SIZE: usize = PageAlignedVirtualAddress::ALIGNMENT;
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
const USER_STACK_SIZE: usize = PAGE_SIZE;

const fn svc(op: SyscallOp) -> u32 {
    0xD400_0001 | ((op as u32) << 5)
}

const SVC_MEMORY_MAP: u32 = svc(SyscallOp::MemoryMap);
const SVC_MEMORY_INSPECT: u32 = svc(SyscallOp::MemoryRegionInspect);
const SVC_OBJECT_SIGNAL: u32 = svc(SyscallOp::ObjectSignal);
const SVC_THREAD_EXIT: u32 = svc(SyscallOp::ThreadExit);

// Регистровые помощники: все movz собираются вручную, чтобы payload был
// детерминирован и независим от какого-либо ассемблера.

/// `movz x{rd}, #{imm}, lsl #{shift*16}` где shift∈{0,1,2,3}.
const fn movz(rd: u32, imm: u16, shift: u32) -> u32 {
    0xD280_0000 | (shift << 21) | ((imm as u32) << 5) | rd
}

/// `movk x{rd}, #{imm}, lsl #{shift*16}`.
const fn movk(rd: u32, imm: u16, shift: u32) -> u32 {
    0xF280_0000 | (shift << 21) | ((imm as u32) << 5) | rd
}

/// `mov x{rd}, x{rm}` (псевдо-инструкция через ORR Xrd, XZR, Xrm).
const fn mov_xrd_xrm(rd: u32, rm: u32) -> u32 {
    0xAA00_03E0 | (rm << 16) | rd
}

/// `str x{rt}, [x{rn}]`.
const fn str_xrt_xrn(rt: u32, rn: u32) -> u32 {
    0xF900_0000 | (rn << 5) | rt
}

/// `cmp x{rn}, x{rm}` = `subs xzr, x{rn}, x{rm}`.
const fn cmp_xn_xm(rn: u32, rm: u32) -> u32 {
    0xEB00_001F | (rm << 16) | (rn << 5)
}

/// `b.ne #+disp_words` (relative, in 4-byte instructions). Положительное
/// смещение — вперёд от текущей инструкции.
const fn b_ne(disp_words: u32) -> u32 {
    let imm19 = disp_words & 0x7FFFF;
    0x5400_0001 | (imm19 << 5)
}

/// `b .` (бесконечный цикл, fallback при возврате).
const B_LOOP: u32 = 0x1400_0000;

const PATTERN_LO: u16 = 0xBABE;
const PATTERN_MID_LO: u16 = 0xCAFE;
const PATTERN_MID_HI: u16 = 0xBEEF;
const PATTERN_HI: u16 = 0xDEAD;

/// HandleId-ы для свежевышедшей таблицы детерминированы:
/// первый insert — slot=0 generation=1 → raw = 0x0001_0000.
/// Второй — slot=1 generation=1 → raw = 0x0001_0001.
const HANDLE_EVENT_RAW_LO: u16 = 0x0001;
const HANDLE_EVENT_RAW_HI: u16 = 0x0001;

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
    let mut words: [u32; 28] = [B_LOOP; 28];
    let mut i = 0usize;

    // mov x21, x0
    words[i] = mov_xrd_xrm(21, 0);
    i += 1;

    // MemoryMap(handle=x21, size=4096, flags=0)
    words[i] = mov_xrd_xrm(0, 21);
    i += 1;
    words[i] = movz(1, 0x1000, 0);
    i += 1;
    words[i] = movz(2, 0, 0);
    i += 1;
    words[i] = SVC_MEMORY_MAP;
    i += 1;

    // mov x19, x0  ; va
    words[i] = mov_xrd_xrm(19, 0);
    i += 1;

    // load pattern in x20
    words[i] = movz(20, PATTERN_LO, 0);
    i += 1;
    words[i] = movk(20, PATTERN_MID_LO, 1);
    i += 1;
    words[i] = movk(20, PATTERN_MID_HI, 2);
    i += 1;
    words[i] = movk(20, PATTERN_HI, 3);
    i += 1;

    // str x20, [x19]
    words[i] = str_xrt_xrn(20, 19);
    i += 1;

    // MemoryRegionInspect(handle=x21)
    words[i] = mov_xrd_xrm(0, 21);
    i += 1;
    words[i] = SVC_MEMORY_INSPECT;
    i += 1;

    // x22 = EXPECTED_SIZE; cmp x0, x22; b.ne to thread_exit (fail path skips signal)
    words[i] = movz(22, EXPECTED_SIZE as u16, 0);
    i += 1;
    words[i] = cmp_xn_xm(0, 22);
    i += 1;
    // skip 7 words (через signal) и попасть на ThreadExit
    words[i] = b_ne(8);
    i += 1;

    // x23 = EXPECTED_INSPECT_SECONDARY; cmp x1, x23; b.ne to thread_exit
    let secondary_lo = EXPECTED_INSPECT_SECONDARY as u16;
    let secondary_hi = (EXPECTED_INSPECT_SECONDARY >> 16) as u16;
    words[i] = movz(23, secondary_lo, 0);
    i += 1;
    words[i] = movk(23, secondary_hi, 1);
    i += 1;
    words[i] = cmp_xn_xm(1, 23);
    i += 1;
    // skip 5 words до ThreadExit
    words[i] = b_ne(5);
    i += 1;

    // ObjectSignal(event_handle, EVENT_SIGNALED, 0)
    words[i] = movz(0, HANDLE_EVENT_RAW_LO, 0);
    i += 1;
    words[i] = movk(0, HANDLE_EVENT_RAW_HI, 1);
    i += 1;
    words[i] = movz(1, 0x0001, 0);
    i += 1;
    words[i] = movz(2, 0, 0);
    i += 1;
    words[i] = SVC_OBJECT_SIGNAL;
    i += 1;

    // ThreadExit(0)
    words[i] = movz(0, 0, 0);
    i += 1;
    words[i] = SVC_THREAD_EXIT;
    i += 1;
    words[i] = B_LOOP;
    i += 1;

    debug_assert_eq!(i, 28);

    let mut bytes = [0u8; 28 * 4];
    for (idx, w) in words.iter().enumerate() {
        bytes[idx * 4..(idx + 1) * 4].copy_from_slice(&w.to_le_bytes());
    }
    bytes
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
    let info = super::user_process_launcher()
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
    USERSPACE_MEMORY_KOBJECT_MAP_AND_INSPECT,
    "userspace_memory_kobject_map_and_inspect",
    userspace_memory_kobject_map_and_inspect
);
