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

use crate::case::{TestCase, TestKind};

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

/// Регистрирует функцию выключения машины. Повторные вызовы игнорируются.
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

/// Завершает прогон с кодом `code` через установленную exit-функцию.
/// Без установленной функции уходит в spin loop.
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
    #[allow(clippy::multiple_unsafe_ops_per_block)]
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

/// Прогоняет зарегистрированные кейсы профиля `kind` и выключает машину.
///
/// Порядок обхода задаётся раскладкой секции `.tests.kernel` линкером и не
/// является частью контракта: кейс обязан быть самодостаточным - сам создавать
/// нужное ему состояние и не полагаться на то, что оставил предыдущий.
pub fn run_tests(kind: TestKind) -> ! {
    let cases = iter_cases();
    let selected = cases.iter().filter(|case| case.kind == kind).count();

    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-RUN: {selected}]");
    });

    for case in cases.iter().filter(|case| case.kind == kind) {
        run_case(case);
    }

    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-DONE: {selected}]");
    });
    exit(0)
}

/// Прогоняет обычный профиль тестов.
pub fn run_all_tests() -> ! {
    run_tests(TestKind::Regular)
}
