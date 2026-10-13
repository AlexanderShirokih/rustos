//! Окна регистров арбитра.

/// Окно регистров арбитра, по имени записи в `reg-names` его узла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// Версия и таблица каналов.
    Core,
    /// Команды каналов на запись.
    Channels,
    /// Команды каналов на чтение.
    Observer,
    /// Разрешение, статус и аккумулятор прерываний каналов.
    Interrupts,
    /// Владельцы каналов.
    Config,
}

impl Window {
    pub(crate) const ALL: [Self; 5] = [
        Self::Core,
        Self::Channels,
        Self::Observer,
        Self::Interrupts,
        Self::Config,
    ];

    /// Возвращает номер окна, различный у разных окон.
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self as usize
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Channels => "chnls",
            Self::Observer => "obsrvr",
            Self::Interrupts => "intr",
            Self::Config => "cnfg",
        }
    }
}
