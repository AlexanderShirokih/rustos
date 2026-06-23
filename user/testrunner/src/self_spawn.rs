//! E2E проверка `ProcessCreate`/`ProcessLoadImage`/`ProcessStart` из EL0:
//! testrunner готовит образ child-процесса через memory-syscall'ы, стартует
//! его и дожидается завершения через bound-`Signal` процесса.

use kernel_tests::kernel_test;
use runtime::{
    memory_create_virtual, memory_map, memory_remap, process_create, process_exit_code,
    process_load_image, process_resource_self, process_start, process_termination_signal,
    signal_wait_one,
};
use syscall::{MEM_FLAGS_READ_WRITE, SIGNALED, SyscallOp};

const PAGE_SIZE: u64 = 4096;

/// Child VA: фиксированный low-half адрес в отдельном AS child'а.
const CHILD_CODE_VA: u64 = 0x2000_0000;
const CHILD_STACK_TOP: u64 = 0x2010_0000;
const CHILD_STACK_SIZE: u64 = PAGE_SIZE;
const CHILD_USER_VM_BASE: u64 = 0x2002_0000;
const CHILD_USER_VM_SIZE: u64 = 0x10_0000;
const CHILD_EXIT_CODE: u32 = 0x55;
const CHILD_WAIT_TIMEOUT_NS: u64 = 500_000_000;

/// `UserMemFlags::ReadExecute` raw-код syscall-ABI.
const MEM_FLAGS_READ_EXECUTE: u64 = 2;
/// `access_mask` региона: R|W|X - после записи кода регион remap'ится в RX.
const ACCESS_RWX: u64 = 0b111;

/// Сериализованные размеры `UserImageDescAbi`/`UserSegmentAbi`.
const IMAGE_DESC_SIZE: usize = 56;
const SEGMENT_SIZE: usize = 32;
const SEGMENT_ABI_VERSION: u32 = 1;

/// Child-код: `movz w0, #CHILD_EXIT_CODE; svc #ThreadExit; b .`.
const CHILD_CODE: [u32; 3] = [
    0x5280_0000 | (CHILD_EXIT_CODE << 5),
    0xD400_0001 | ((SyscallOp::ThreadExit as u32) << 5),
    0x1400_0000,
];

/// Little-endian сериализация `UserSegmentAbi`: region_handle, flags,
/// va_base, mapped_size, reserved.
fn encode_segment(region_handle: u32) -> [u8; SEGMENT_SIZE] {
    let mut bytes = [0u8; SEGMENT_SIZE];
    bytes[0..4].copy_from_slice(&region_handle.to_le_bytes());
    bytes[4..8].copy_from_slice(&(MEM_FLAGS_READ_EXECUTE as u32).to_le_bytes());
    bytes[8..16].copy_from_slice(&CHILD_CODE_VA.to_le_bytes());
    bytes[16..24].copy_from_slice(&PAGE_SIZE.to_le_bytes());
    bytes
}

/// Little-endian сериализация `UserImageDescAbi`: version, segment_count,
/// segments_va, entry_va, stack top/size, user_vm base/size.
fn encode_image_desc(segments_va: u64) -> [u8; IMAGE_DESC_SIZE] {
    let mut bytes = [0u8; IMAGE_DESC_SIZE];
    bytes[0..4].copy_from_slice(&SEGMENT_ABI_VERSION.to_le_bytes());
    bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
    bytes[8..16].copy_from_slice(&segments_va.to_le_bytes());
    bytes[16..24].copy_from_slice(&CHILD_CODE_VA.to_le_bytes());
    bytes[24..32].copy_from_slice(&CHILD_STACK_TOP.to_le_bytes());
    bytes[32..40].copy_from_slice(&CHILD_STACK_SIZE.to_le_bytes());
    bytes[40..48].copy_from_slice(&CHILD_USER_VM_BASE.to_le_bytes());
    bytes[48..56].copy_from_slice(&CHILD_USER_VM_SIZE.to_le_bytes());
    bytes
}

#[kernel_test]
fn self_spawn_via_syscalls() {
    // Регион child-кода: маппим RW, пишем инструкции, поднимаем в RX.
    let resource = process_resource_self().expect("metering resource handle");
    let region = memory_create_virtual(resource, PAGE_SIZE, ACCESS_RWX).expect("region handle");
    let va = memory_map(region, PAGE_SIZE, MEM_FLAGS_READ_WRITE);
    kernel_tests::kassert!(va > 0);
    let code_va = u64::try_from(va).expect("positive va fits u64");
    let code = usize::try_from(va).expect("positive va fits usize") as *mut u32;
    // SAFETY: MemoryMap выдал RW-маппинг размером PAGE_SIZE; три слова
    // child-кода лежат в его границах.
    unsafe {
        for (i, word) in CHILD_CODE.iter().enumerate() {
            code.add(i).write_volatile(*word);
        }
    }
    kernel_tests::kassert_eq!(memory_remap(code_va, PAGE_SIZE, MEM_FLAGS_READ_EXECUTE), 0);

    let segment = encode_segment(region.raw());
    let desc = encode_image_desc(segment.as_ptr() as u64);

    let child = process_create(b"child").expect("child process handle");
    kernel_tests::kassert_eq!(process_load_image(child, &desc), 0);

    // priority = 1, bootstrap-handle'ов нет.
    process_start(child, CHILD_CODE_VA, CHILD_STACK_TOP, 0, 1, 0).expect("child thread handle");

    let term_signal = process_termination_signal(child).expect("process_termination_signal");
    let observed = signal_wait_one(term_signal, SIGNALED, CHILD_WAIT_TIMEOUT_NS);
    kernel_tests::kassert_eq!(observed, i64::from(SIGNALED));
    kernel_tests::kassert_eq!(process_exit_code(child), i64::from(CHILD_EXIT_CODE));
}
