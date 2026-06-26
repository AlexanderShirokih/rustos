//! Поглощение хэндлов в `Process::start` на ошибочном пути из EL0: процесс без
//! загруженного образа отвергает `start`, переданный по значению хэндл
//! закрывается обёрткой (его `Drop`) ровно один раз, не утекая.

use kernel_tests::kernel_test;
use runtime::{Priority, Process, Signal, ThreadEntry, handle_close, signal_set};
use syscall::{Handle, SIGNALED, SyscallError, WakeCount};

/// `true`, если non-blocking операция на `handle` отвергнута как `BadHandle`
/// (хэндл закрыт).
fn is_closed(handle: Handle) -> bool {
    signal_set(handle, SIGNALED, 0, WakeCount::None) == SyscallError::BadHandle.as_return_value()
}

#[kernel_test]
fn start_error_closes_passed_handles() {
    let process = Process::create("start-error").expect("process create must succeed");

    let owned = Signal::create().expect("signal create").into_handle();
    let stale = owned.as_raw();

    let entry = ThreadEntry {
        entry_pc: 0,
        user_sp: 0,
        arg: 0,
        priority: Priority::new(0),
    };

    // Процесс без load_image не стартует: start возвращает ошибку.
    let error = process.start(entry, owned).err();
    kernel_tests::kassert!(error.is_some());

    kernel_tests::kassert!(is_closed(stale));
    kernel_tests::kassert_eq!(
        handle_close(stale),
        SyscallError::BadHandle.as_return_value()
    );
}
