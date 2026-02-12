//! Крейт логирования ядра.
//!
//! Предоставляет макросы для вывода сообщений с уровнями логирования
//! и поддержку раннего вывода до включения MMU.

#![no_std]
extern crate alloc;

use collections::{LockCell, MutexCell, NoLockCell};
use core::fmt::{Arguments, Write as _};
use core::sync::atomic::{AtomicBool, Ordering};
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

/// Держатель консольного вывода с поддержкой раннего и нормального режимов.
struct ConsoleHolder {
    /// Ранний writer (до включения MMU). Запись/чтение однопоточные.
    early: NoLockCell<&'static StaticWriter>,
    /// Нормальный writer (после включения MMU).
    normal: MutexCell<&'static StaticWriter>,
    /// Флаг перехода в нормальный режим.
    is_normal: AtomicBool,
}

impl ConsoleHolder {
    pub const fn new() -> Self {
        Self {
            early: NoLockCell::new(&NIL),
            normal: MutexCell::new(&NIL),
            is_normal: AtomicBool::new(false),
        }
    }

    fn with_writer<R>(&self, fun: impl FnOnce(&mut &'static StaticWriter) -> R) -> R {
        if self.is_normal.load(Ordering::Acquire) {
            self.normal.with_lock(fun)
        } else {
            self.early.with_lock(fun)
        }
    }
}

// SAFETY: `UnsafeCell` делает тип !Sync, но мы гарантируем корректность через режимы:
// - до MMU запись/чтение однопоточно;
// - после MMU ранний writer больше не меняется, а вывод идет через `normal` mutex.
unsafe impl Sync for ConsoleHolder {}

static STDOUT: ConsoleHolder = ConsoleHolder::new();

struct FmtWriter<'a>(&'a StaticWriter);

impl core::fmt::Write for FmtWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.write_all(s.as_bytes());
        Ok(())
    }
}

/// Вызывается на раннем этапе (однопоточно). Разрешено вызвать только один раз.
pub fn set_early_stdout(w: alloc::boxed::Box<StaticWriter>) {
    let leaked: &'static StaticWriter = alloc::boxed::Box::leak(w);
    STDOUT.early.with_lock(|writer| *writer = leaked);
}

/// Вызывается после включения MMU.
/// Можно вызывать повторно, чтобы заменить writer.
pub fn set_stdout(w: &'static StaticWriter) {
    STDOUT.normal.with_lock(|writer| *writer = w);

    // Публикация перехода в normal mode после установки writer
    STDOUT.is_normal.store(true, Ordering::Release);
}

/// Возвращает текущий early writer, если он установлен.
pub fn get_early_writer() -> Option<&'static StaticWriter> {
    STDOUT.early.with_lock(|w| {
        if core::ptr::eq(*w, &NIL) {
            None
        } else {
            Some(*w)
        }
    })
}

/// Печать форматированной строки без префикса уровня.
pub fn printf(arguments: Arguments) {
    STDOUT.with_writer(|writer| {
        let mut fmt = FmtWriter(*writer);
        let _ = fmt.write_fmt(arguments);
        writer.flush();
    })
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

    printf(format_args!("{}{}\r\n", level, arguments));
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

    printf(format_args!("{}[{}] {}\r\n", level, tag, arguments));
}

/// Возвращает имя файла без пути для использования в качестве тега.
pub fn file_tag(path: &str) -> &str {
    path.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(path)
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
