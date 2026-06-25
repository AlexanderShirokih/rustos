//! E2E проверка `ProcessCreate`/`ProcessLoadImage`/`ProcessStart` из EL0:
//! E2E проверка `ProcessCreate`/`ProcessLoadImage`/`ProcessStart` из EL0.

use alloc::vec;

use kernel_tests::kernel_test;
use runtime::{
    Mapping, MemoryAccess, MemoryRegion, Priority, Process, Resource, Signal, ThreadEntry, Timeout,
    UserMemFlags, handle_close,
};
use syscall::{Handle, SyscallError, SyscallOp};

const PAGE_SIZE: u64 = 4096;

/// Child VA: фиксированный low-half адрес в отдельном AS child'а.
const CHILD_CODE_VA: u64 = 0x2000_0000;
const CHILD_STACK_TOP: u64 = 0x2010_0000;
const CHILD_STACK_SIZE: u64 = PAGE_SIZE;
// Окно user_vm-аллокатора - "дыра" между концом сегмента кода и базой стека
// (stack_base = CHILD_STACK_TOP - CHILD_STACK_SIZE = 0x200F_F000); не должно
// пересекать ни сегмент, ни стек, как и production-план build_user_vm_allocator.
const CHILD_USER_VM_BASE: u64 = 0x2002_0000;
const CHILD_USER_VM_SIZE: u64 = 0xC_0000;
const CHILD_EXIT_CODE: u32 = 0x55;
const CHILD_WAIT_TIMEOUT_NS: u64 = 500_000_000;

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
    bytes[4..8].copy_from_slice(&(UserMemFlags::ReadExecute.raw() as u32).to_le_bytes());
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

/// `ThreadEntry` первого потока child'а: вход и стек из image-дескриптора.
fn child_entry() -> ThreadEntry {
    ThreadEntry {
        entry_pc: CHILD_CODE_VA,
        user_sp: CHILD_STACK_TOP,
        arg: 0,
        priority: Priority::new(1),
    }
}

/// Готовит загруженный child-процесс. Возвращает регион и маппинг кода
/// отдельно: оба независимы и должны жить до старта child'а, иначе обёртки
/// закроют region handle или снимут маппинг до того, как ядро прочитает их.
fn load_child() -> (Process, MemoryRegion, Mapping) {
    let resource = Resource::self_resource().expect("metering resource");
    let region = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RWX)
        .expect("code region");
    let mapping = region
        .map(PAGE_SIZE, UserMemFlags::ReadWrite)
        .expect("code mapping");

    let code = usize::try_from(mapping.va()).expect("positive va fits usize") as *mut u32;
    // SAFETY: map выдал RW-маппинг размером PAGE_SIZE; три слова child-кода
    // лежат в его границах.
    unsafe {
        for (i, word) in CHILD_CODE.iter().enumerate() {
            code.add(i).write_volatile(*word);
        }
    }
    mapping
        .remap(UserMemFlags::ReadExecute)
        .expect("remap to RX");

    let segment = encode_segment(region.handle().as_raw().raw());
    let desc = encode_image_desc(segment.as_ptr() as u64);

    let process = Process::create("child").expect("child process");
    process.load_image(&desc).expect("load image");

    (process, region, mapping)
}

#[kernel_test]
fn self_spawn_via_syscalls() {
    let (process, _region, _mapping) = load_child();

    let thread = process.start(child_entry(), vec![]).expect("child thread");
    drop(thread);

    process
        .join(Timeout::from_ns(CHILD_WAIT_TIMEOUT_NS))
        .expect("child terminates");
    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), CHILD_EXIT_CODE);
}

/// `true`, если non-blocking `handle_close` на `handle` отвергнут как
/// `BadHandle` (хэндл не в таблице родителя).
fn is_absorbed(handle: Handle) -> bool {
    handle_close(handle) == SyscallError::BadHandle.as_return_value()
}

#[kernel_test]
fn start_success_absorbs_handles() {
    let (process, _region, _mapping) = load_child();

    let owned = Signal::create().expect("signal create").into_handle();
    let stale = owned.as_raw();

    // child-код игнорирует bootstrap-хэндл; signal несёт TRANSFER по умолчанию.
    let thread = process
        .start(child_entry(), vec![owned])
        .expect("child thread");

    // Дожидаемся завершения child (успех старта), затем освобождаем все хэндлы
    // родителя, полученные на/после изъятия stale, чтобы их слоты не могли
    // совпасть с освободившимся слотом stale в момент проверки.
    process
        .join(Timeout::from_ns(CHILD_WAIT_TIMEOUT_NS))
        .expect("child terminates");
    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), CHILD_EXIT_CODE);
    drop(thread);

    kernel_tests::kassert!(is_absorbed(stale));
}
