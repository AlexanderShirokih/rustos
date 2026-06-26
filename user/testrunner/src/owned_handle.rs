//! Инварианты владения `OwnedHandle` из EL0: хэндлы минтятся через
//! `signal_create`, проверка закрытия - `signal_set` (non-blocking) на сыром
//! id, который после закрытия даёт `BadHandle`.

use kernel_tests::kernel_test;
use runtime::{OwnedHandle, handle_close, signal_create, signal_set};
use syscall::{Handle, Rights, SIGNALED, SyscallError, WakeCount};

/// Минтит свежий `Signal` и оборачивает во владеющий хэндл.
fn mint() -> OwnedHandle {
    let handle = signal_create().expect("signal_create must succeed");
    // SAFETY: handle только что создан syscall'ом, мы единственный владелец.
    unsafe { OwnedHandle::from_handle(handle) }
}

/// `true`, если non-blocking операция на `handle` отвергнута как `BadHandle`
/// (хэндл закрыт).
fn is_closed(handle: Handle) -> bool {
    signal_set(handle, SIGNALED, 0, WakeCount::None) == SyscallError::BadHandle.as_return_value()
}

#[kernel_test]
fn drop_closes_handle() {
    let owned = mint();
    let stale = owned.as_raw();
    // Хэндл жив до drop.
    kernel_tests::kassert_eq!(signal_set(stale, SIGNALED, 0, WakeCount::None), 0);

    drop(owned);

    kernel_tests::kassert!(is_closed(stale));
}

#[kernel_test]
fn into_raw_suppresses_drop() {
    let owned = mint();
    let raw = owned.into_raw();

    // into_raw подавил Drop: хэндл ещё жив, нужен ручной close.
    kernel_tests::kassert_eq!(signal_set(raw, SIGNALED, 0, WakeCount::None), 0);
    kernel_tests::kassert_eq!(handle_close(raw), 0);
    // Повторный close уже закрытого id - BadHandle.
    kernel_tests::kassert_eq!(handle_close(raw), SyscallError::BadHandle.as_return_value());
}

#[kernel_test]
fn close_does_not_double_close() {
    let owned = mint();
    let stale = owned.as_raw();

    owned.close().expect("close must succeed");

    // close закрыл хэндл: операция на сыром id - BadHandle.
    kernel_tests::kassert!(is_closed(stale));
    kernel_tests::kassert_eq!(
        handle_close(stale),
        SyscallError::BadHandle.as_return_value()
    );
}

#[kernel_test]
fn duplicate_is_independent() {
    let original = mint();
    let duplicate = original
        .duplicate(Rights::READ | Rights::WRITE | Rights::DUPLICATE, 0)
        .expect("duplicate must succeed");
    let original_raw = original.as_raw();

    // Drop дубликата не трогает оригинал.
    drop(duplicate);

    kernel_tests::kassert_eq!(signal_set(original_raw, SIGNALED, 0, WakeCount::None), 0);

    original.close().expect("original close must succeed");
}

#[kernel_test]
fn borrow_duplicate_is_independent() {
    let original = mint();
    let duplicate = original
        .borrow()
        .duplicate(Rights::READ | Rights::WRITE | Rights::DUPLICATE, 0)
        .expect("borrow duplicate must succeed");
    let original_raw = original.as_raw();

    // Дубликат поверх заимствования независим: его Drop не трогает оригинал.
    drop(duplicate);

    kernel_tests::kassert_eq!(signal_set(original_raw, SIGNALED, 0, WakeCount::None), 0);

    original.close().expect("original close must succeed");
}
