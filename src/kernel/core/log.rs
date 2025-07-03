use crate::kernel::core::streams::{OutputStream, OutputStreamExt};
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

/// Macro for logging a message with a specific log level
#[macro_export]
macro_rules! log_line {
    ($level:expr, $message:expr) => {
        $crate::kernel::core::log::log("[");
        $crate::kernel::core::log::log($level);
        $crate::kernel::core::log::log("] ");
        $crate::kernel::core::log::log($message);
        $crate::kernel::core::log::log("\n\r");
    };
}

/// Convenience macro for debug logging
#[macro_export]
macro_rules! log_debug {
    ($message:expr) => {
        $crate::log_line!("D", $message);
    };
}

/// Convenience macro for info logging
#[macro_export]
macro_rules! log_info {
    ($message:expr) => {
        $crate::log_line!("I", $message);
    };
}

/// Convenience macro for warning logging
#[macro_export]
macro_rules! log_warning {
    ($message:expr) => {
        $crate::log_line!("W", $message);
    };
}

/// Convenience macro for error logging
#[macro_export]
macro_rules! log_error {
    ($message:expr) => {
        $crate::log_line!("E", $message);
    };
}

/// Logger trait for system-wide logging
///
/// This trait defines methods for logging at different levels:
/// - error: Critical errors that require immediate attention
/// - warning: Potential issues that might lead to errors
/// - info: General information about system operation
/// - debug: Detailed information for debugging purposes
pub trait Logger {
    /// Log an error message
    fn error(&mut self, message: &str);

    /// Log a warning message
    fn warning(&mut self, message: &str);

    /// Log an informational message
    fn info(&mut self, message: &str);

    /// Log a debug message
    fn debug(&mut self, message: &str);

    fn log(&mut self, message: &str);
}

/// Logger implementation that uses any type implementing OutputStream
pub struct OutputStreamLogger<'a> {
    pub stream: &'a mut dyn OutputStream<WriteError = ()>,
}

impl<'a> OutputStreamLogger<'a> {
    pub fn new(stream: &'a mut dyn OutputStream<WriteError = ()>) -> Self {
        Self { stream }
    }
}

impl<'a> Logger for OutputStreamLogger<'a> {
    fn error(&mut self, message: &str) {
        log_error!(message);
    }

    fn warning(&mut self, message: &str) {
        log_warning!(message);
    }

    fn info(&mut self, message: &str) {
        log_info!(message);
    }

    fn debug(&mut self, message: &str) {
        log_debug!(message);
    }

    fn log(&mut self, message: &str) {
        let _ = self.stream.write_str(message);
    }
}

// Global logger instance for system-wide logging
static mut STATIC_LOGGER: Option<&mut dyn Logger> = None;

/// Initialize the global logger with a logger instance
///
/// This function accepts any type that implements the Logger trait.
pub fn set_logger(logger: &OutputStreamLogger) {
    unsafe {
        STATIC_LOGGER = Some(logger);
    }
}

struct GlobalLogger;

/// Log an error message to the global logger
pub fn error(message: &str) {
    unsafe {
        if let Some(ref mut logger) = STATIC_LOGGER {
            logger.error(message);
        }
    }
}

/// Log a warning message to the global logger
pub fn warning(message: &str) {
    unsafe {
        if let Some(ref mut logger) = STATIC_LOGGER {
            logger.warning(message);
        }
    }
}

/// Log an informational message to the global logger
pub fn info(message: &str) {
    unsafe {
        if let Some(ref mut logger) = STATIC_LOGGER {
            logger.info(message);
        }
    }
}

/// Log a debug message to the global logger
pub fn debug(message: &str) {
    unsafe {
        if let Some(ref mut logger) = STATIC_LOGGER {
            logger.debug(message);
        }
    }
}

/// Log a message to the global logger
pub fn log(message: &str) {
    unsafe {
        if let Some(ref mut logger) = STATIC_LOGGER {
            logger.log(message);
        }
    }
}
