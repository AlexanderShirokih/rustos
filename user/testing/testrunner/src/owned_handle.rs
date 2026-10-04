//! Инварианты владения `OwnedHandle` из EL0: хэндлы минтятся через
//! `signal_create`, проверка закрытия - `signal_set` (non-blocking) на сыром
//! id, который после закрытия даёт `BadHandle`.

use kernel_objects::{Signal, kind};
use kernel_tests::kernel_test;
use runtime::signal_set;
use syscall::{Handle, Rights, SIGNALED, SyscallError, WakeCount};

fn mint() -> Signal {
    Signal::create().expect("signal create must succeed")
}

fn is_closed(handle: Handle<kind::Signal>) -> bool {
    signal_set(handle, SIGNALED, 0, WakeCount::None) == SyscallError::BadHandle.as_return_value()
}

#[kernel_test]
fn drop_closes_handle() {
    let owned = mint();
    let stale = owned.as_raw();
    kernel_tests::kassert_eq!(signal_set(stale, SIGNALED, 0, WakeCount::None), 0);

    drop(owned);

    kernel_tests::kassert!(is_closed(stale));
}

#[kernel_test]
fn into_raw_suppresses_drop() {
    let owned = mint();
    let raw = owned.into_raw();

    kernel_tests::kassert_eq!(signal_set(raw, SIGNALED, 0, WakeCount::None), 0);
}

#[kernel_test]
fn close_does_not_double_close() {
    let owned = mint();
    let stale = owned.as_raw();

    owned.close().expect("close must succeed");

    kernel_tests::kassert!(is_closed(stale));
}

#[kernel_test]
fn duplicate_is_independent() {
    let original = mint();
    let duplicate = original
        .duplicate(Rights::READ | Rights::WRITE | Rights::DUPLICATE, None)
        .expect("duplicate must succeed");
    let original_raw = original.as_raw();

    drop(duplicate);

    kernel_tests::kassert_eq!(signal_set(original_raw, SIGNALED, 0, WakeCount::None), 0);

    original.close().expect("original close must succeed");
}

#[kernel_test]
fn borrow_duplicate_is_independent() {
    let original = mint();
    let duplicate = original
        .borrow()
        .duplicate(Rights::READ | Rights::WRITE | Rights::DUPLICATE, None)
        .expect("borrow duplicate must succeed");
    let original_raw = original.as_raw();

    drop(duplicate);

    kernel_tests::kassert_eq!(signal_set(original_raw, SIGNALED, 0, WakeCount::None), 0);

    original.close().expect("original close must succeed");
}
