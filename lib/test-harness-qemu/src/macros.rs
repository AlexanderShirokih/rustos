//! `kassert!`/`kassert_eq!`/`kassert_ne!`/`kreport!`/`kskip!`, [`fail`] и [`skip`].

use core::fmt::{Arguments, Write as _};

use crate::runner::{FmtAdapter, exit, mark_skipped, with_writer};

pub fn report(line: Arguments<'_>) {
    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-REPORT] {line}");
    });
}

pub fn fail(reason: Arguments<'_>, file: &'static str, line: u32) -> ! {
    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-FAIL: {reason}] at {file}:{line}");
    });
    exit(1)
}

/// Отмечает текущий тест пропущенным с причиной `reason`.
///
/// После вызова тест обязан вернуться. Макрос [`kskip!`](crate::kskip) возвращается за него.
pub fn skip(reason: Arguments<'_>, file: &'static str, line: u32) {
    with_writer(|w| {
        let mut fmt = FmtAdapter::new(w);
        let _ = writeln!(fmt, "[TEST-SKIP: {reason}] at {file}:{line}");
    });
    mark_skipped();
}

/// Печатает причину пропуска текущего теста и возвращается из тестовой функции.
///
/// Применяется, когда на машине нет условия теста, например описанного устройства.
#[macro_export]
macro_rules! kskip {
    ($($arg:tt)+) => {{
        $crate::macros::skip(::core::format_args!($($arg)+), file!(), line!());
        return;
    }};
}

#[macro_export]
macro_rules! kreport {
    ($($arg:tt)+) => {
        $crate::macros::report(::core::format_args!($($arg)+));
    };
}

#[macro_export]
macro_rules! kassert {
    ($cond:expr $(,)?) => {
        if !($cond) {
            $crate::macros::fail(
                ::core::format_args!(concat!("kassert ", stringify!($cond))),
                file!(),
                line!(),
            );
        }
    };
    ($cond:expr, $($arg:tt)+) => {
        if !($cond) {
            $crate::macros::fail(::core::format_args!($($arg)+), file!(), line!());
        }
    };
}

#[macro_export]
macro_rules! kassert_eq {
    ($left:expr, $right:expr $(,)?) => {{
        let left_val = &($left);
        let right_val = &($right);
        if left_val != right_val {
            $crate::macros::fail(
                ::core::format_args!(
                    "kassert_eq {} != {} (left={:?} right={:?})",
                    stringify!($left),
                    stringify!($right),
                    left_val,
                    right_val,
                ),
                file!(),
                line!(),
            );
        }
    }};
}

#[macro_export]
macro_rules! kassert_ne {
    ($left:expr, $right:expr $(,)?) => {{
        let left_val = &($left);
        let right_val = &($right);
        if left_val == right_val {
            $crate::macros::fail(
                ::core::format_args!(
                    "kassert_ne {} == {} (left={:?} right={:?})",
                    stringify!($left),
                    stringify!($right),
                    left_val,
                    right_val,
                ),
                file!(),
                line!(),
            );
        }
    }};
}
