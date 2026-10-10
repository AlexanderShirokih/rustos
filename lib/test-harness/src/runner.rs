//! Прогон тест-кейсов и печать маркеров.
//!
//! Контракт маркеров:
//! - `[TEST-RUN: <count>]`, `[TEST-START: name]`, `[TEST-PASS: name]`,
//!   `[TEST-DONE: <count>]` - успех;
//! - `[TEST-SKIP: <reason>] at <file>:<line>` вместо `[TEST-PASS: name]` - пропуск теста,
//!   начатого последним `[TEST-START]`;
//! - `[TEST-FAIL: <reason>] at <file>:<line>` - провал.

#![allow(unsafe_code)]

use core::{
    fmt::Write as _,
    str::Utf8Error,
    sync::atomic::{AtomicBool, Ordering},
};

use io::writer::Writer;
use spin::Once;
use userland_image::DecodedImage;

use crate::{
    case::{TestCase, TestKind},
    macros::fail,
};

const FILTER_IMAGE_PATH: &str = "/testrunner/test-filter";

#[allow(improper_ctypes)]
unsafe extern "C" {
    static __tests_start: TestCase;
    static __tests_end: TestCase;
}

static WRITER: Once<&'static (dyn Writer + Send + Sync)> = Once::new();
static EXIT: Once<fn(u32) -> !> = Once::new();
static SKIPPED: AtomicBool = AtomicBool::new(false);

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
    let start = core::ptr::addr_of!(__tests_start);
    let end = core::ptr::addr_of!(__tests_end);
    // SAFETY: оба символа экспортирует линкер-скрипт тестового бинаря, они обрамляют секцию
    // `.tests` и лежат в одном образе.
    let len = usize::try_from(unsafe { end.offset_from(start) }).unwrap_or(0);
    // SAFETY: секция хранит `len` статиков `TestCase`, размещённых линкером подряд, и остаётся
    // отображённой до конца прогона.
    unsafe { core::slice::from_raw_parts(start, len) }
}

pub(crate) fn mark_skipped() {
    SKIPPED.store(true, Ordering::Relaxed);
}

fn run_case(case: &TestCase) {
    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-START: {}]", case.name);
    });

    (case.run)();

    if SKIPPED.swap(false, Ordering::Relaxed) {
        return;
    }

    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-PASS: {}]", case.name);
    });
}

/// Прогоняет зарегистрированные кейсы профиля `kind` и выключает машину.
///
/// Фильтр по подстроке имени кейса читается из `image`.
///
/// Порядок обхода задаётся раскладкой секции `.tests` линкером и не
/// является частью контракта. Кейс обязан быть самодостаточным - сам создавать
/// нужное ему состояние и не полагаться на то, что оставил предыдущий.
pub fn run_tests(kind: TestKind, image: Option<&DecodedImage<'_>>) -> ! {
    let filter = match image.map(image_filter).transpose() {
        Ok(filter) => filter.flatten(),
        Err(_) => fail(
            format_args!("test filter at {FILTER_IMAGE_PATH} is not UTF-8"),
            file!(),
            line!(),
        ),
    };

    let cases = iter_cases();
    let selected = cases
        .iter()
        .filter(|case| is_selected(case, kind, filter))
        .count();

    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-RUN: {selected}]");
    });

    for case in cases.iter().filter(|case| is_selected(case, kind, filter)) {
        run_case(case);
    }

    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-DONE: {selected}]");
    });
    exit(0)
}

fn is_selected(case: &TestCase, kind: TestKind, filter: Option<&str>) -> bool {
    case.kind == kind && filter.is_none_or(|filter| case.name.contains(filter))
}

fn image_filter<'a>(image: &DecodedImage<'a>) -> Result<Option<&'a str>, Utf8Error> {
    image
        .entry(FILTER_IMAGE_PATH)
        .map(|entry| core::str::from_utf8(entry.data()))
        .transpose()
}

#[cfg(test)]
mod tests {
    use userland_image::{ImageRecord, decode, encode};

    use super::{FILTER_IMAGE_PATH, image_filter, is_selected};
    use crate::case::{TestCase, TestKind};

    #[test]
    fn a_filter_selects_cases_whose_name_contains_it() {
        let case = |name| TestCase {
            name,
            run: || {},
            kind: TestKind::Regular,
        };

        let filter = Some("liveness");
        assert!(is_selected(
            &case("port_liveness_on_close"),
            TestKind::Regular,
            filter
        ));
        assert!(!is_selected(&case("port_send"), TestKind::Regular, filter));
    }

    #[test]
    fn the_filter_is_read_from_its_image_record() {
        let records = [
            ImageRecord {
                path: "/testrunner",
                data: b"boot",
            },
            ImageRecord {
                path: FILTER_IMAGE_PATH,
                data: b"liveness",
            },
        ];
        let bytes = encode(&records, "/testrunner").expect("the image encodes");
        let image = decode(&bytes).expect("the image decodes");

        assert_eq!(image_filter(&image), Ok(Some("liveness")));
    }
}
