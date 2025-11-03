use crate::io::writer::Writer;
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
    fn print(&self, s: &str);
    fn log(&self, level: Level, s: &str);
}

struct Nil;
static NIL_CONSOLE: Nil = Nil;
impl Console for Nil {
    fn print(&self, _: &str) {}
    fn log(&self, _: Level, _: &str) {}
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
    fn print(&self, s: &str) {
        self.w.write_all(s.as_bytes());
        self.w.flush();
    }

    fn log(&self, lvl: Level, s: &str) {
        self.print(match lvl {
            Level::Fatal => "[FATAL] ",
            Level::Error => "[E] ",
            Level::Warn => "[W] ",
            Level::Info => "[I] ",
            Level::Debug => "[D] ",
        });
        self.print(s);
        self.print("\r\n");
    }
}

/// Адаптер под core::fmt::Write, чтобы можно было использовать write!()/format_args!() без alloc.
pub struct Fmt<'a, C: Console>(pub &'a C);
impl<'a, C: Console> core::fmt::Write for Fmt<'a, C> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.print(s);
        Ok(())
    }
}

static CONS: Once<&'static dyn Console> = Once::new();

#[inline(always)]
pub fn set_console(c: &'static dyn Console) {
    let _ = CONS.call_once(|| c);
}

#[inline(always)]
pub fn get_console() -> &'static dyn Console {
    CONS.get().copied().unwrap_or(&NIL_CONSOLE)
}

#[inline(always)]
pub fn fatal(message: &str) {
    get_console().log(Level::Fatal, message)
}
#[inline(always)]
pub fn error(message: &str) {
    get_console().log(Level::Error, message)
}
#[inline(always)]
pub fn warn(message: &str) {
    get_console().log(Level::Warn, message)
}
#[inline(always)]
pub fn info(message: &str) {
    get_console().log(Level::Info, message)
}
#[inline(always)]
pub fn debug(message: &str) {
    get_console().log(Level::Debug, message)
}

#[inline(always)]
pub fn print(message: &str) {
    get_console().print(message)
}
