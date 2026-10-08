//! E2E проверка self-handle части Process/Thread ABI из EL0:
//! `Process::self_process`/`Thread::self_thread` возвращают валидные обёртки,
//! `close` закрывает их без ошибок.

use kernel_objects::{Process, Thread};

#[test_harness::test]
fn process_and_thread_self_handles() {
    let process = Process::self_process().expect("process_self handle");
    process.close().expect("process handle close");

    let thread = Thread::self_thread().expect("thread_self handle");
    thread.close().expect("thread handle close");
}
