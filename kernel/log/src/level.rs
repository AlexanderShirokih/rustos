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

pub(crate) fn prefix(level: Level) -> &'static str {
    match level {
        Level::Fatal => "[FATAL] ",
        Level::Error => "[E] ",
        Level::Warn => "[W] ",
        Level::Info => "[I] ",
        Level::Debug => "[D] ",
    }
}
