use crate::io::writer::Writer;
use core::fmt::{Arguments, Write};
use spin::Mutex;

pub enum Level {
    Fatal,
    Error,
    Warn,
    Info,
    Debug,
}

/// Консоль для вывода форматированных сообщений
pub trait Console: Sync {
    fn printf(&self, arguments: Arguments);
    fn logf(&self, level: Level, arguments: Arguments);
}

struct NilWriter;

static NIL_WRITER: NilWriter = NilWriter;
static STDOUT: Stdout = Stdout {
    writer: Mutex::new(&NIL_WRITER),
};

impl Writer for NilWriter {
    fn write_all(&self, _bytes: &[u8]) {}

    fn flush(&self) {}
}

/// Адаптер для использования `&dyn Writer` с `core::fmt::Write`
struct WriterAdapter<'a>(&'a dyn Writer);

impl Write for WriterAdapter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.write_all(s.as_bytes());
        Ok(())
    }
}

impl Console for Stdout {
    fn printf(&self, arguments: Arguments) {
        WriterAdapter(*self.writer.lock())
            .write_fmt(arguments)
            .unwrap()
    }

    fn logf(&self, lvl: Level, arguments: Arguments) {
        let level = match lvl {
            Level::Fatal => "[FATAL] ",
            Level::Error => "[E] ",
            Level::Warn => "[W] ",
            Level::Info => "[I] ",
            Level::Debug => "[D] ",
        };

        self.printf(format_args!("{}{}\r\n", level, arguments));
    }
}

pub struct Stdout {
    writer: Mutex<&'static dyn Writer>,
}

unsafe impl Sync for Stdout {}
unsafe impl Send for Stdout {}

impl Write for &Stdout {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        WriterAdapter(*self.writer.lock()).write_str(s)
    }
}

pub fn set_stdout(w: &'static dyn Writer) {
    *STDOUT.writer.lock() = w;
}

pub fn stdout() -> &'static Stdout {
    &STDOUT
}

pub fn log_fmt(level: Level, args: Arguments) {
    stdout().logf(level, args)
}

#[macro_export]
macro_rules! fatal {
    ($console:expr, $($arg:tt)*) => {
        $crate::console::Console::logf($console, $crate::console::Level::Fatal, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! error {
    ($console:expr, $($arg:tt)*) => {
        $crate::console::Console::logf($console, $crate::console::Level::Error, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! warn {
    ($console:expr, $($arg:tt)*) => {
        $crate::console::Console::logf($console, $crate::console::Level::Warn, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! info {
    ($console:expr, $($arg:tt)*) => {
        $crate::console::Console::logf($console, $crate::console::Level::Info, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! debug {
    ($console:expr, $($arg:tt)*) => {
        $crate::console::Console::logf($console, $crate::console::Level::Debug, format_args!($($arg)*))
    };
}
