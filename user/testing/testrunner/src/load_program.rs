//! E2E проверка загрузчика: загрузка синтетической программы, передача bootstrap-хэндла и
//! локализация исключения в дочернем процессе.
//!
//! Машинный код фикстур использует AArch64, соответствующий target этого
//! `testrunner`.

use kernel_objects::{Process, Resource, Signal};
use kernel_tests::kernel_test;
use runtime::{Timeout, signal_set};
use syscall::{ProcessName, SIGNALED, SyscallError, SyscallOp, WakeCount};
use userland::{Program, Segment, SegmentPermissions};
use userland_loader::load_program;

const PAGE_SIZE: u64 = syscall::PAGE_SIZE as u64;
const CHILD_CODE_VA: u64 = 0x2000_0000;
const CHILD_WAIT_TIMEOUT: Timeout = Timeout::from_millis(500);

fn clean_exit_child_code() -> [u8; 12] {
    // AArch64: `svc HandleClose; svc ThreadExit; b .` закрывает bootstrap-хэндл,
    // переданный в x0, и публикует код возврата `HandleClose` как exit-код.
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

fn data_abort_child_code() -> [u8; 12] {
    // AArch64: `mov x0, xzr; ldr x0, [x0]; b .` вызывает Data Abort чтением
    // незамапленной VA 0.
    let words: [u32; 3] = [0xAA1F_03E0, 0xF940_0000, 0x1400_0000];
    let mut bytes = [0u8; 12];
    for (i, word) in words.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

fn load_synthetic_child(
    resource: &Resource,
    name: &str,
    code: &[u8],
    bootstrap: Signal,
) -> Process {
    let segments = [Segment {
        va_base: CHILD_CODE_VA,
        mem_size: PAGE_SIZE,
        permissions: SegmentPermissions::ReadExecute,
        bytes: code,
    }];
    let program = Program {
        manifest: &[],
        entry_va: CHILD_CODE_VA,
        stack_size: PAGE_SIZE,
        segments: &segments,
    };
    load_program(
        &program,
        ProcessName::new(name).expect("fixture process name"),
        resource,
        bootstrap,
    )
    .expect("load_program spawns child")
}

#[kernel_test]
fn load_program_passes_bootstrap_handle() {
    let resource = Resource::self_resource().expect("self metering resource");
    let code = clean_exit_child_code();
    let bootstrap = Signal::create().expect("signal create");
    let passed = bootstrap.as_raw();
    let process = load_synthetic_child(&resource, "child", &code, bootstrap);

    kernel_tests::kassert_eq!(
        signal_set(passed, SIGNALED, 0, WakeCount::None),
        SyscallError::BadHandle.as_return_value()
    );
    process.join(CHILD_WAIT_TIMEOUT).expect("child terminates");
    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);
}

#[kernel_test]
fn synchronous_fault_terminates_child_and_runner_continues() {
    let resource = Resource::self_resource().expect("self metering resource");
    let fault_code = data_abort_child_code();
    let faulting = load_synthetic_child(
        &resource,
        "faulting-child",
        &fault_code,
        Signal::create().expect("signal create"),
    );

    faulting
        .join(CHILD_WAIT_TIMEOUT)
        .expect("faulting child terminates");
    kernel_tests::kassert!(faulting.exit_code().expect("fault exit code") != 0);

    let canary_code = clean_exit_child_code();
    let canary = load_synthetic_child(
        &resource,
        "post-fault-canary",
        &canary_code,
        Signal::create().expect("signal create"),
    );
    canary
        .join(CHILD_WAIT_TIMEOUT)
        .expect("post-fault child terminates");
    kernel_tests::kassert_eq!(canary.exit_code().expect("canary exit code"), 0);
}
