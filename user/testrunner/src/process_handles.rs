//! E2E проверка self-handle части Process/Thread ABI из EL0:
//! `ProcessSelf`/`ThreadSelf` возвращают ненулевые handle'ы,
//! `HandleClose` закрывает их без ошибок.

use kernel_tests::kernel_test;
use runtime::{handle_close, process_self, thread_self};

#[kernel_test]
fn process_and_thread_self_handles() {
    let process = process_self().expect("process_self handle");
    kernel_tests::kassert_eq!(handle_close(process), 0);

    let thread = thread_self().expect("thread_self handle");
    kernel_tests::kassert_eq!(handle_close(thread), 0);
}
