pub use log_abi::Level;

pub(crate) fn prefix(level: Level) -> &'static str {
    match level {
        Level::Fatal => "[FATAL] ",
        Level::Error => "[E] ",
        Level::Warn => "[W] ",
        Level::Info => "[I] ",
        Level::Debug => "[D] ",
    }
}
