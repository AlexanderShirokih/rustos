//! Прогон тест-кейсов и печать маркеров.
//!
//! Контракт маркеров:
//! - `[TEST-RUN: <count>]`, `[TEST-START: name]`, `[TEST-PASS: name]`,
//!   `[TEST-DONE: <count>]` - успех;
//! - `[TEST-FAIL: <reason>] at <file>:<line>` - провал.

use core::fmt::Write as _;

use io::writer::Writer;
use spin::Once;

use crate::{backend::Backend, case::TestCase};

#[allow(improper_ctypes)]
unsafe extern "C" {
    static __tests_kernel_start: TestCase;
    static __tests_kernel_end: TestCase;
}

static WRITER: Once<&'static (dyn Writer + Send + Sync)> = Once::new();
static BACKEND: Once<&'static (dyn Backend + 'static)> = Once::new();

pub fn install_writer(writer: &'static (dyn Writer + Send + Sync)) {
    WRITER.call_once(|| writer);
}

pub fn install_backend(backend: &'static (dyn Backend + 'static)) {
    BACKEND.call_once(|| backend);
}

pub fn with_writer<F>(f: F)
where
    F: FnOnce(&dyn Writer),
{
    if let Some(writer) = WRITER.get() {
        f(*writer);
    }
}

pub fn with_backend<F, R>(f: F) -> R
where
    F: FnOnce(&dyn Backend) -> R,
{
    let backend = *BACKEND
        .get()
        .expect("backend was not installed via install_backend");
    f(backend)
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

pub fn run_all_tests() -> ! {
    let cases = iter_cases();

    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-RUN: {}]", cases.len());
    });

    for case in cases {
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

    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-DONE: {}]", cases.len());
    });
    with_backend(|b| b.exit(0))
}
