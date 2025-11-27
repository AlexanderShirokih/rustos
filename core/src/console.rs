use crate::io::writer::Writer;
use core::fmt::Arguments;
use spin::Once;

pub enum Level {
    Fatal,
    Error,
    Warn,
    Info,
    Debug,
}

/// Logger trait for system-wide logging
pub trait Console: Sync {
    fn printf(&self, arguments: Arguments);
    fn logf(&self, level: Level, arguments: Arguments);
}

#[derive(Copy, Clone)]
struct Nil;
static NIL_CONSOLE: Nil = Nil;
impl Console for Nil {
    fn printf(&self, _: Arguments) {}
    fn logf(&self, _: Level, _: Arguments) {}
}

pub struct BasicConsole<W: Writer> {
    w: W,
}

impl<W: Writer> BasicConsole<W> {
    pub fn new(w: W) -> Self {
        Self { w }
    }
}

impl<W: Writer + Sync> Console for BasicConsole<W> {
    fn printf(&self, arguments: Arguments) {
        if let Some(s) = arguments.as_str() {
            self.w.write_all(s.as_bytes());
            self.w.flush();
        } else {
            use core::fmt::Write;
            use heapless::String;
            let mut buffer = String::<256>::new();
            if buffer.write_fmt(arguments).is_ok() {
                self.w.write_all(buffer.as_bytes());
                self.w.flush();
            }
        }
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

static CONS: Once<&'static dyn Console> = Once::new();

pub fn set_stdout(c: &'static dyn Console) {
    let _ = CONS.call_once(|| c);
}

#[inline(always)]
pub fn stdout() -> &'static dyn Console {
    CONS.get().copied().unwrap_or(&NIL_CONSOLE)
}

pub fn log_fmt(level: Level, args: Arguments) {
    stdout().logf(level, args)
}

#[macro_export]
macro_rules! printf {
    ($console:expr, $($arg:tt)*) => {
        $console.printf(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! fatal {
    ($console:expr, $($arg:tt)*) => {
        $console.logf($crate::console::Level::Fatal, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! error {
    ($console:expr, $($arg:tt)*) => {
        $console.logf($crate::console::Level::Error, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! warn {
    ($console:expr, $($arg:tt)*) => {
        $console.logf($crate::console::Level::Warn, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! info {
    ($console:expr, $($arg:tt)*) => {
        $console.logf($crate::console::Level::Info, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! debug {
    ($console:expr, $($arg:tt)*) => {
        $console.logf($crate::console::Level::Debug, format_args!($($arg)*))
    };
}
