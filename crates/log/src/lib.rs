//! Крейт логирования ядра.
//!
//! Предоставляет макросы для вывода сообщений с уровнями логирования.

#![no_std]
extern crate alloc;

use core::fmt::{Arguments, Write as _};

use collections::{LockCell, MutexCell};
use io::writer::Writer;

/// Тип статического writer'а для вывода логов.
type StaticWriter = dyn Writer + Sync + 'static;

/// Уровень важности сообщения.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Level {
    /// Критическая ошибка, требующая остановки системы.
    Fatal,
    /// Ошибка, не требующая остановки.
    Error,
    /// Предупреждение.
    Warn,
    /// Информационное сообщение.
    Info,
    /// Отладочное сообщение.
    Debug,
}

struct NilWriter;
impl Writer for NilWriter {
    fn write_all(&self, _: &[u8]) {}
    fn flush(&self) {}
}

static NIL: NilWriter = NilWriter;

static STDOUT: MutexCell<&'static StaticWriter> = MutexCell::new(&NIL);

struct FmtWriter<'a>(&'a StaticWriter);

impl core::fmt::Write for FmtWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.write_all(s.as_bytes());
        Ok(())
    }
}

/// Устанавливает writer для вывода логов.
/// Можно вызывать повторно, чтобы заменить writer.
pub fn set_stdout(w: &'static StaticWriter) {
    STDOUT.with_lock(|writer| *writer = w);
}

/// Печать форматированной строки без префикса уровня.
pub fn printf(arguments: Arguments) {
    STDOUT.with_lock(|writer| {
        let mut fmt = FmtWriter(*writer);
        let _ = fmt.write_fmt(arguments);
        writer.flush();
    });
}

/// Лог с префиксом уровня и переводом строки.
pub fn logf(lvl: Level, arguments: Arguments) {
    let level = match lvl {
        Level::Fatal => "[FATAL] ",
        Level::Error => "[E] ",
        Level::Warn => "[W] ",
        Level::Info => "[I] ",
        Level::Debug => "[D] ",
    };

    printf(format_args!("{level}{arguments}\r\n"));
}

/// Лог с префиксом уровня, тегом и переводом строки.
pub fn logf_tagged(lvl: Level, tag: &str, arguments: Arguments) {
    let level = match lvl {
        Level::Fatal => "[FATAL] ",
        Level::Error => "[E] ",
        Level::Warn => "[W] ",
        Level::Info => "[I] ",
        Level::Debug => "[D] ",
    };

    printf(format_args!("{level}[{tag}] {arguments}\r\n"));
}

/// Возвращает имя файла без пути для использования в качестве тега.
pub fn file_tag(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

#[macro_export]
macro_rules! fatal {
    ($tag:expr; $($arg:tt)*) => {
        $crate::logf_tagged($crate::Level::Fatal, $tag, format_args!($($arg)*))
    };
    ($($arg:tt)*) => {
        $crate::logf_tagged(
            $crate::Level::Fatal,
            $crate::file_tag(file!()),
            format_args!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! error {
    ($tag:expr; $($arg:tt)*) => {
        $crate::logf_tagged($crate::Level::Error, $tag, format_args!($($arg)*))
    };
    ($($arg:tt)*) => {
        $crate::logf_tagged(
            $crate::Level::Error,
            $crate::file_tag(file!()),
            format_args!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! warn {
    ($tag:expr; $($arg:tt)*) => {
        $crate::logf_tagged($crate::Level::Warn, $tag, format_args!($($arg)*))
    };
    ($($arg:tt)*) => {
        $crate::logf_tagged(
            $crate::Level::Warn,
            $crate::file_tag(file!()),
            format_args!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! info {
    ($tag:expr; $($arg:tt)*) => {
        $crate::logf_tagged($crate::Level::Info, $tag, format_args!($($arg)*))
    };
    ($($arg:tt)*) => {
        $crate::logf_tagged(
            $crate::Level::Info,
            $crate::file_tag(file!()),
            format_args!($($arg)*),
        )
    };
}

#[macro_export]
macro_rules! debug {
    ($tag:expr; $($arg:tt)*) => {
        $crate::logf_tagged($crate::Level::Debug, $tag, format_args!($($arg)*))
    };
    ($($arg:tt)*) => {
        $crate::logf_tagged(
            $crate::Level::Debug,
            $crate::file_tag(file!()),
            format_args!($($arg)*),
        )
    };
}
