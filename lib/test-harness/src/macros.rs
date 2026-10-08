//! `check!`/`check_eq!`/`check_ne!`/`report!`/`skip!`, [`fail`] и [`skip`].

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
/// После вызова тест обязан вернуться. Макрос [`skip!`](crate::skip) возвращается за него.
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
macro_rules! skip {
    ($($arg:tt)+) => {{
        $crate::macros::skip(::core::format_args!($($arg)+), file!(), line!());
        return;
    }};
}

#[macro_export]
macro_rules! report {
    ($($arg:tt)+) => {
        $crate::macros::report(::core::format_args!($($arg)+));
    };
}

#[macro_export]
macro_rules! check {
    ($cond:expr $(,)?) => {
        if !($cond) {
            $crate::macros::fail(
                ::core::format_args!(concat!("check ", stringify!($cond))),
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
macro_rules! check_eq {
    ($left:expr, $right:expr $(,)?) => {{
        let left_val = &($left);
        let right_val = &($right);
        if left_val != right_val {
            $crate::macros::fail(
                ::core::format_args!(
                    "check_eq {} != {} (left={:?} right={:?})",
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
macro_rules! check_ne {
    ($left:expr, $right:expr $(,)?) => {{
        let left_val = &($left);
        let right_val = &($right);
        if left_val == right_val {
            $crate::macros::fail(
                ::core::format_args!(
                    "check_ne {} == {} (left={:?} right={:?})",
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
