//! E2E завершения из EL0: `ThreadExit(N)` помечает объект завершённым и
//! публикует `N` как exit-код - и для потока текущего процесса, и для
//! процесса-потомка целиком.

use kernel_tests::kernel_test;
use runtime::{Priority, Process, Resource, Thread, ThreadEntry, UserMemFlags, thread_exit};

use crate::{
    fixture::{Fixture, JOIN_TIMEOUT},
    fixture_protocol::{OP_EXIT, REPLY_OK},
};

/// Размер стека рабочего потока (page-aligned выдача -> вершина 16-байт-выровнена).
const STACK_SIZE: u64 = 0x4000;
/// Код, которым завершаются и поток, и потомок.
const EXIT_CODE: u32 = 42;

/// Точка входа рабочего потока: сразу уходит в `ThreadExit(arg)`.
extern "C" fn exiting_entry(code: usize) -> ! {
    thread_exit(code as u64)
}

/// Поток, вызвавший `ThreadExit(N)`, наблюдается терминированным с кодом `N`.
///
/// Поток создаётся напрямую через `Thread::create`, а не через `spawn`:
/// `JoinHandle::join` поглощает дескриптор, а тесту нужен exit-код уже
/// после присоединения.
#[kernel_test]
fn thread_exit_publishes_exit_code() {
    let stack = Resource::self_resource()
        .expect("self metering resource")
        .allocate(STACK_SIZE, UserMemFlags::ReadWrite)
        .expect("allocate worker stack");
    let process = Process::self_process().expect("self process handle");

    let entry: extern "C" fn(usize) -> ! = exiting_entry;
    let thread = Thread::create(
        process.handle(),
        ThreadEntry {
            entry_pc: entry as *const () as u64,
            user_sp: stack.va() + STACK_SIZE,
            arg: u64::from(EXIT_CODE),
            priority: Priority::new(1),
        },
    )
    .expect("create worker thread");

    thread
        .join(JOIN_TIMEOUT)
        .expect("worker terminates before timeout");
    kernel_tests::kassert_eq!(thread.exit_code().expect("thread exit code"), EXIT_CODE);
}

/// Потомок, завершившийся `ThreadExit(N)`, наблюдается родителем как
/// терминированный процесс с тем же кодом.
#[kernel_test]
fn child_process_exit_code_is_observable() {
    let fixture = Fixture::spawn();

    let status = fixture.request_status(OP_EXIT, u64::from(EXIT_CODE), &[]);
    kernel_tests::kassert_eq!(status, REPLY_OK);

    fixture
        .process()
        .join(JOIN_TIMEOUT)
        .expect("child terminates");
    kernel_tests::kassert_eq!(
        fixture.process().exit_code().expect("child exit code"),
        EXIT_CODE
    );
}
