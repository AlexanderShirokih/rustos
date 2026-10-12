/// Уровень важности строки лога.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
#[repr(u8)]
pub enum Level {
    /// Критическая ошибка, требующая остановки системы.
    Fatal = 0,
    /// Ошибка, не требующая остановки.
    Error = 1,
    /// Предупреждение.
    Warn = 2,
    /// Информационное сообщение.
    Info = 3,
    /// Отладочное сообщение.
    Debug = 4,
}

impl Level {
    pub(crate) const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Fatal),
            1 => Some(Self::Error),
            2 => Some(Self::Warn),
            3 => Some(Self::Info),
            4 => Some(Self::Debug),
            _ => None,
        }
    }
}
