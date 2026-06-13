//! Прогон тест-кейсов и печать маркеров.
//!
//! Контракт маркеров:
//! - `[TEST-RUN: <count>]`, `[TEST-START: name]`, `[TEST-PASS: name]`,
//!   `[TEST-DONE: <count>]` - успех;
//! - `[TEST-FAIL: <reason>] at <file>:<line>` - провал.

#![allow(unsafe_code)]

use core::fmt::Write as _;

use io::writer::Writer;
use spin::Once;

use crate::case::TestCase;

#[allow(improper_ctypes)]
unsafe extern "C" {
    static __tests_kernel_start: TestCase;
    static __tests_kernel_end: TestCase;
}

static WRITER: Once<&'static (dyn Writer + Send + Sync)> = Once::new();
static EXIT: Once<fn(u32) -> !> = Once::new();

pub fn install_writer(writer: &'static (dyn Writer + Send + Sync)) {
    WRITER.call_once(|| writer);
}

/// Регистрирует функцию выключения машины; повторные вызовы игнорируются.
pub fn install_exit(exit: fn(u32) -> !) {
    EXIT.call_once(|| exit);
}

pub fn with_writer<F>(f: F)
where
    F: FnOnce(&dyn Writer),
{
    if let Some(writer) = WRITER.get() {
        f(*writer);
    }
}

/// Завершает прогон с кодом `code` через установленную exit-функцию;
/// без установленной функции уходит в spin loop.
pub fn exit(code: u32) -> ! {
    if let Some(exit) = EXIT.get() {
        exit(code);
    }
    loop {
        core::hint::spin_loop();
    }
}

pub struct FmtAdapter<'a> {
    inner: &'a dyn Writer,
}

impl<'a> FmtAdapter<'a> {
    pub fn new(inner: &'a dyn Writer) -> Self {
        Self { inner }
    }
}

impl core::fmt::Write for FmtAdapter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.inner.write_all(s.as_bytes());
        Ok(())
    }
}

fn iter_cases() -> &'static [TestCase] {
    // SAFETY: символы экспортируются линкер-скриптом тестового бинаря и
    // обрамляют секцию `.tests.kernel` со статиками, размещёнными
    // `register_test!`. `end >= start` по конструкции скрипта.
    unsafe {
        let start = core::ptr::addr_of!(__tests_kernel_start);
        let end = core::ptr::addr_of!(__tests_kernel_end);
        let len = usize::try_from(end.offset_from(start)).unwrap_or(0);
        core::slice::from_raw_parts(start, len)
    }
}

fn run_case(case: &TestCase) {
    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-START: {}]", case.name);
    });
    (case.run)();
    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-PASS: {}]", case.name);
    });
}

fn should_run_early(name: &str) -> bool {
    matches!(
        name,
        "userspace_eret_to_el0_invokes_dispatcher"
            | "userspace_spawn_user_process_runs_to_exit"
            | "userspace_vm_allocate_and_remap"
            | "userspace_vm_allocate_free_reuse_va"
            | "syscall_from_kernel_origin_is_rejected"
            | "event_signal_after_deadline"
            | "process_lifecycle_exit_code_and_termination_signals"
            | "userland_bootstrap_handshake"
    )
}

pub fn run_all_tests() -> ! {
    let cases = iter_cases();

    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-RUN: {}]", cases.len());
    });

    for case in cases {
        // Эти кейсы либо аллоцируют полноценные user-process'ы и отдельные AS,
        // либо чувствительны к позднему запуску после долгих stateful test'ов.
        // Поднимаем их в начало, чтобы убрать зависимость от link-order.
        if should_run_early(case.name) {
            run_case(case);
        }
    }

    for case in cases {
        if !should_run_early(case.name) {
            run_case(case);
        }
    }

    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-DONE: {}]", cases.len());
    });
    exit(0)
}
