use core::fmt::{Arguments, Write};
use io::writer::Writer;
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

/// Адаптер для использования `&dyn Writer` с `kernel::fmt::Write`
struct WriterAdapter<'a>(&'a dyn Writer);

impl Write for WriterAdapter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.write_all(s.as_bytes());
        Ok(())
    }
}

impl Console for Stdout {
    fn printf(&self, arguments: Arguments) {
        let guard = self.writer;
        let mut adapter = WriterAdapter(guard);
        adapter.write_fmt(arguments).unwrap();
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

struct StdoutHolder {
    inner: Mutex<Stdout>,
}

#[derive(Copy, Clone)]
pub struct Stdout {
    writer: &'static dyn Writer,
}

unsafe impl Sync for Stdout {}
unsafe impl Send for Stdout {}

impl Write for Stdout {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let guard = self.writer;
        let mut adapter = WriterAdapter(guard);
        adapter.write_str(s)
    }
}

/// Nil writer that does nothing - used as default
struct NilWriter;

impl Writer for NilWriter {
    fn write_all(&self, _data: &[u8]) {}
    fn flush(&self) {}
}

static CURRENT_STDOUT: StdoutHolder = StdoutHolder {
    inner: Mutex::new(Stdout { writer: &NilWriter }),
};

pub fn set_stdout(w: &'static dyn Writer) {
    CURRENT_STDOUT.inner.lock().writer = w;
}

pub fn stdout() -> Stdout {
    *CURRENT_STDOUT.inner.lock()
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
        $crate::console::Console::logf(&$console, $crate::console::Level::Error, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! warn {
    ($console:expr, $($arg:tt)*) => {
        $crate::console::Console::logf(&$console, $crate::console::Level::Warn, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! info {
    ($console:expr, $($arg:tt)*) => {
        $crate::console::Console::logf(&$console, $crate::console::Level::Info, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! debug {
    ($console:expr, $($arg:tt)*) => {
        $crate::console::Console::logf(&$console, $crate::console::Level::Debug, format_args!($($arg)*))
    };
}
