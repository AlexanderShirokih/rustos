//! E2E проверка `userland_loader::load_entry` из EL0: загрузка синтетической
//! программы из `EntryView` и передача ей bootstrap-хэндла в `x0`.

use kernel_tests::kernel_test;
use runtime::{Resource, Signal, Timeout};
use syscall::SyscallOp;
use userland::{Entry, Segment, SegmentPermissions};
use userland_loader::load_entry;

const PAGE_SIZE: u64 = 4096;
const CHILD_CODE_VA: u64 = 0x2000_0000;
const CHILD_WAIT_TIMEOUT_NS: u64 = 500_000_000;

/// Child-код: `svc HandleClose; svc ThreadExit; b .`. На входе `x0` несёт
/// child-table id переданного bootstrap-хэндла; `HandleClose` закрывает его и
/// кладёт результат в `x0`, `ThreadExit` выходит с этим кодом. Валидный хэндл
/// -> close вернул 0 -> exit 0; нулевой/битый x0 дал бы ненулевой код.
fn child_code() -> [u8; 12] {
    let words: [u32; 3] = [
        0xD400_0001 | ((SyscallOp::HandleClose as u32) << 5),
        0xD400_0001 | ((SyscallOp::ThreadExit as u32) << 5),
        0x1400_0000,
    ];
    let mut bytes = [0u8; 12];
    for (i, word) in words.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

#[kernel_test]
fn load_entry_passes_bootstrap_handle() {
    let resource = Resource::self_resource();
    let code = child_code();
    let segments = [Segment {
        va_base: CHILD_CODE_VA,
        mem_size: PAGE_SIZE,
        permissions: SegmentPermissions::ReadExecute,
        bytes: &code,
    }];
    let entry = Entry {
        name: "child",
        entry_va: CHILD_CODE_VA,
        stack_size: PAGE_SIZE,
        segments: &segments,
    };
    let bootstrap = Signal::create().expect("signal create").into_handle();
    let process =
        load_entry(&entry, &resource, bootstrap).expect("load_entry spawns child");

    process
        .join(Timeout::from_ns(CHILD_WAIT_TIMEOUT_NS))
        .expect("child terminates");
    // x0 нёс валидный child-table id -> HandleClose вернул 0.
    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);
}
