//! End-to-end проверка syscall-слоя через kernel-side `SVC`.
//!
//! Диспатчер обрабатывает SVC одинаково независимо от уровня, поэтому
//! kernel-thread может покрыть весь путь "vector -> exception_entry ->
//! syscall::dispatch -> handler -> kobject" простым `svc #imm`. EL0-вход
//! отдельно проверяется в [`super::userspace_entry`].

use core::arch::asm;

use drivers_common::services::scheduler::{Priority, SchedulerServiceExt, SpawnConfig};
use main::{
    kobject::{EVENT_SIGNALED, Event, Handle, KObject, Rights, handle_close, install_handle},
    syscall::{SyscallError, SyscallOp},
    syscall_bridge::scheduler,
};
use qemu_test_harness::register_test;

/// SVC с неизвестным immediate должен возвращать `-BadSyscall`,
/// kernel - продолжить выполнение без panic.
fn syscall_unknown_op_returns_bad_syscall() {
    let result: i64;
    // SAFETY: неизвестный SVC immediate, диспатчер кодирует ошибку.
    unsafe {
        asm!(
            "svc #99",
            lateout("x0") result,
            options(nostack, preserves_flags),
        );
    }
    qemu_test_harness::kassert_eq!(result, i64::from(SyscallError::BadSyscall));
}

/// `object_signal` через SVC: kernel создаёт Event, инсталлит handle,
/// сигналит через SVC. Затем читаем состояние Event напрямую - оно
/// должно содержать поднятый бит.
fn syscall_object_signal_round_trip() {
    let event = Event::new();
    let handle = Handle::new(
        KObject::Event(event.clone()),
        Rights::SIGNAL | Rights::WAIT | Rights::INSPECT,
    );
    let id = install_handle(handle).expect("install_handle must succeed");
    let raw = u64::from(id.raw().get());

    let result: i64;
    // SAFETY: regs x0..x2 переданы по AAPCS64, syscall возвращает в x0.
    unsafe {
        asm!(
            "svc #{op}",
            in("x0") raw,
            in("x1") u64::from(EVENT_SIGNALED),
            in("x2") 0_u64,
            lateout("x0") result,
            op = const SyscallOp::ObjectSignal as u16,
            options(nostack, preserves_flags),
        );
    }
    qemu_test_harness::kassert_eq!(result, 0);
    qemu_test_harness::kassert!(event.peek() & EVENT_SIGNALED == EVENT_SIGNALED);
}

/// `object_wait_one` через SVC, fast-path: сигналим Event до wait;
/// диспатчер возвращает наблюдённую маску.
fn syscall_object_wait_one_fast_path() {
    let event = Event::new();
    event.signal(EVENT_SIGNALED, 0);
    let handle = Handle::new(
        KObject::Event(event.clone()),
        Rights::WAIT | Rights::INSPECT,
    );
    let id = install_handle(handle).expect("install_handle must succeed");
    let raw = u64::from(id.raw().get());

    let observed: i64;
    // SAFETY: см. syscall_object_signal_round_trip.
    unsafe {
        asm!(
            "svc #{op}",
            in("x0") raw,
            in("x1") u64::from(EVENT_SIGNALED),
            in("x2") 0_u64,
            lateout("x0") observed,
            op = const SyscallOp::ObjectWaitOne as u16,
            options(nostack, preserves_flags),
        );
    }
    qemu_test_harness::kassert_eq!(observed, i64::from(EVENT_SIGNALED));
}

/// `object_wait_one` через SVC, poll-path: Event не сигналит,
/// `timeout_ns = 0` должен вернуть `Timeout`, не блокируя поток.
fn syscall_object_wait_one_poll_returns_timeout() {
    let event = Event::new();
    let handle = Handle::new(
        KObject::Event(event.clone()),
        Rights::WAIT | Rights::INSPECT,
    );
    let id = install_handle(handle).expect("install_handle must succeed");
    let raw = u64::from(id.raw().get());

    let observed: i64;
    // SAFETY: см. syscall_object_signal_round_trip.
    unsafe {
        asm!(
            "svc #{op}",
            in("x0") raw,
            in("x1") u64::from(EVENT_SIGNALED),
            in("x2") 0_u64,
            lateout("x0") observed,
            op = const SyscallOp::ObjectWaitOne as u16,
            options(nostack, preserves_flags),
        );
    }
    qemu_test_harness::kassert_eq!(observed, i64::from(SyscallError::Timeout));
    handle_close(id).expect("close poll-test handle");
}

/// `object_wait_one` через SVC, slow-path: сигнал поднимается отдельным
/// thread'ом после `sleep_ms`. Диспатчер должен корректно проводить
/// блокировку через scheduler и вернуть маску после wake-up.
fn syscall_object_wait_one_blocks_until_signaled() {
    let event = Event::new();
    let handle = Handle::new(
        KObject::Event(event.clone()),
        Rights::WAIT | Rights::SIGNAL | Rights::INSPECT,
    );
    let id = install_handle(handle).expect("install_handle must succeed");
    let raw = u64::from(id.raw().get());

    let event_for_signal = event.clone();
    let scheduler_for_signal = scheduler().clone();
    scheduler()
        .spawn(
            SpawnConfig::new("syscall-test-signaler").priority(Priority::highest()),
            move || {
                scheduler_for_signal.sleep_ms(20);
                event_for_signal.signal(EVENT_SIGNALED, 0);
            },
        )
        .expect("signaler spawn must succeed");

    let observed: i64;
    // SAFETY: см. syscall_object_signal_round_trip. Таймаут заведомо
    // больше задержки signaler-потока.
    unsafe {
        asm!(
            "svc #{op}",
            in("x0") raw,
            in("x1") u64::from(EVENT_SIGNALED),
            in("x2") 1_000_000_000_u64,
            lateout("x0") observed,
            op = const SyscallOp::ObjectWaitOne as u16,
            options(nostack, preserves_flags),
        );
    }
    qemu_test_harness::kassert_eq!(observed, i64::from(EVENT_SIGNALED));
}

/// SVC с нулевым `HandleId` должен возвращать `-InvalidArgument`.
fn syscall_object_signal_zero_handle_returns_invalid_argument() {
    let result: i64;
    // SAFETY: x0=0 (нулевой HandleId), syscall детерминированно возвращает ошибку.
    unsafe {
        asm!(
            "svc #{op}",
            in("x0") 0_u64,
            in("x1") 1_u64,
            in("x2") 0_u64,
            lateout("x0") result,
            op = const SyscallOp::ObjectSignal as u16,
            options(nostack, preserves_flags),
        );
    }
    qemu_test_harness::kassert_eq!(result, i64::from(SyscallError::InvalidArgument));
}

register_test!(
    SYSCALL_UNKNOWN_OP,
    "syscall_unknown_op_returns_bad_syscall",
    syscall_unknown_op_returns_bad_syscall
);
register_test!(
    SYSCALL_OBJECT_SIGNAL_ROUND_TRIP,
    "syscall_object_signal_round_trip",
    syscall_object_signal_round_trip
);
register_test!(
    SYSCALL_OBJECT_WAIT_ONE_FAST_PATH,
    "syscall_object_wait_one_fast_path",
    syscall_object_wait_one_fast_path
);
register_test!(
    SYSCALL_OBJECT_WAIT_ONE_POLL_TIMEOUT,
    "syscall_object_wait_one_poll_returns_timeout",
    syscall_object_wait_one_poll_returns_timeout
);
register_test!(
    SYSCALL_OBJECT_WAIT_ONE_BLOCKS,
    "syscall_object_wait_one_blocks_until_signaled",
    syscall_object_wait_one_blocks_until_signaled
);
register_test!(
    SYSCALL_OBJECT_SIGNAL_ZERO_HANDLE,
    "syscall_object_signal_zero_handle_returns_invalid_argument",
    syscall_object_signal_zero_handle_returns_invalid_argument
);
