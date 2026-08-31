//! E2E проверка загрузчика: загрузка синтетической программы, передача bootstrap-хэндла,
//! локализация исключения в дочернем процессе и значок retained startup-порта.
//!
//! Машинный код фикстур использует AArch64, соответствующий target этого
//! `testrunner`.

use kernel_objects::{Process, Resource, Signal};
use kernel_tests::kernel_test;
use runtime::Timeout;
use syscall::SyscallOp;
use userland::{Entry, Segment, SegmentPermissions};
use userland_loader::load_entry;

use crate::fixture::{Fixture, JOIN_TIMEOUT};

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

fn load_synthetic_child(resource: &Resource, path: &str, code: &[u8]) -> Process {
    let segments = [Segment {
        va_base: CHILD_CODE_VA,
        mem_size: PAGE_SIZE,
        permissions: SegmentPermissions::ReadExecute,
        bytes: code,
    }];
    let entry = Entry {
        path,
        entry_va: CHILD_CODE_VA,
        stack_size: PAGE_SIZE,
        segments: &segments,
    };
    let bootstrap = Signal::create().expect("signal create").into_handle();
    load_entry(&entry, None, resource, bootstrap).expect("load_entry spawns child")
}

#[kernel_test]
fn load_entry_passes_bootstrap_handle() {
    let resource = Resource::self_resource().expect("self metering resource");
    let code = clean_exit_child_code();
    let process = load_synthetic_child(&resource, "/child", &code);

    process.join(CHILD_WAIT_TIMEOUT).expect("child terminates");
    // x0 нёс валидный child-table id -> HandleClose вернул 0.
    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);
}

#[kernel_test]
fn synchronous_fault_terminates_child_and_runner_continues() {
    let resource = Resource::self_resource().expect("self metering resource");
    let fault_code = data_abort_child_code();
    let faulting = load_synthetic_child(&resource, "/faulting-child", &fault_code);

    faulting
        .join(CHILD_WAIT_TIMEOUT)
        .expect("faulting child terminates");
    kernel_tests::kassert!(faulting.exit_code().expect("fault exit code") != 0);

    let canary_code = clean_exit_child_code();
    let canary = load_synthetic_child(&resource, "/post-fault-canary", &canary_code);
    canary
        .join(CHILD_WAIT_TIMEOUT)
        .expect("post-fault child terminates");
    kernel_tests::kassert_eq!(canary.exit_code().expect("canary exit code"), 0);
}

#[kernel_test]
fn retained_startup_port_reports_the_gone_launcher() {
    // Стартовый кадр доставлен и обслужен, значит сессия под lease-значком жива.
    let fixture = Fixture::spawn_leased();

    // Клиентскую сторону порта закрывает уход `Fixture`, и потомок остаётся без держателей
    // своего значка.
    let process = fixture.into_process();

    process
        .join(JOIN_TIMEOUT)
        .expect("child leaves with its launcher");
    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);
}
