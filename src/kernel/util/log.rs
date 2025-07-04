use crate::kernel::core::streams::{OutputStream, OutputStreamExt};
use spin::Mutex;

/// Log an error message
pub fn error(message: &str) {
    get_logger().log(Level::Error, message);
}

/// Log a warning message
pub fn warn(message: &str) {
    get_logger().log(Level::Warning, message);
}

/// Log an info message
pub fn info(message: &str) {
    get_logger().log(Level::Info, message);
}

/// Log a debug message
pub fn debug(message: &str) {
    get_logger().log(Level::Debug, message);
}

pub enum Level {
    Error,
    Warning,
    Info,
    Debug,
    Message,
}

/// Logger trait for system-wide logging
pub trait Logger {
    /// Log a debug message
    fn log(&self, level: Level, message: &str);

    /// Print custom message
    fn print(&self, message: &str);
}

pub struct OutputStreamLogger {
    stream: &'static dyn OutputStream<WriteError = ()>,
}

impl OutputStreamLogger {
    pub fn new(stream: &'static dyn OutputStream<WriteError = ()>) -> Self {
        Self { stream }
    }
}

unsafe impl Send for OutputStreamLogger {}
unsafe impl Sync for OutputStreamLogger {}

impl Logger for OutputStreamLogger {
    fn log(&self, level: Level, message: &str) {
        match level {
            Level::Error => self.print("[E]"),
            Level::Warning => self.print("[W]"),
            Level::Info => self.print("[I]"),
            Level::Debug => self.print("[D]"),
            Level::Message => (),
        }

        match level {
            Level::Message => self.print(message),
            _ => {
                self.print(" ");
                self.print(message);
                self.print("\n\r");
            }
        };
    }

    fn print(&self, message: &str) {
        let _ = self.stream.write_str(message);
    }
}

// Global logger instance for system-wide logging
static GLOBAL_LOGGER: Mutex<Option<&'static OutputStreamLogger>> = Mutex::new(None);

/// Initialize the global logger with a logger instance
pub(crate) fn set_logger(logger: &'static OutputStreamLogger) {
    GLOBAL_LOGGER.lock().replace(logger);
}

pub fn get_logger() -> &'static OutputStreamLogger {
    GLOBAL_LOGGER.lock().as_ref().unwrap()
}

/// Log a message to the global logger
pub fn print(message: &str) {
    get_logger().print(message)
}
