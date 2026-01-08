#![no_std]
extern crate alloc;

use collections::{LockCell, MutexCell, NoLockCell};
use core::fmt::{Arguments, Write as _};
use core::sync::atomic::{AtomicBool, Ordering};
use io::writer::Writer;

type StaticWriter = dyn Writer + Sync + 'static;

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Level {
    Fatal,
    Error,
    Warn,
    Info,
    Debug,
}

struct NilWriter;
impl Writer for NilWriter {
    fn write_all(&self, _: &[u8]) {}
    fn flush(&self) {}
}

static NIL: NilWriter = NilWriter;

struct ConsoleHolder {
    /// Ранний writer (до включения MMU). Запись/чтение предполагается однопоточной.
    early: NoLockCell<&'static StaticWriter>,
    /// Нормальный writer (после включения MMU).
    normal: MutexCell<&'static StaticWriter>,
    /// Флаг перехода в normal mode.
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

    // Публикуем переход в normal mode после установки writer.
    STDOUT.is_normal.store(true, Ordering::Release);
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

#[macro_export]
macro_rules! fatal {
    ($($arg:tt)*) => {
        $crate::logf($crate::Level::Fatal, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {
        $crate::logf($crate::Level::Error, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::logf($crate::Level::Warn, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::logf($crate::Level::Info, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        $crate::logf($crate::Level::Debug, format_args!($($arg)*))
    };
}
