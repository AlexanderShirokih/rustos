//! Доступ к 32-битным регистрам окон арбитра.

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

/// Регистры окон арбитра. Реализация задаёт размер окна и доступ к его 32-битным регистрам.
/// Смещение в [`load`](Registers::load) и [`store`](Registers::store) выровнено по регистру и
/// лежит внутри окна.
pub trait Registers {
    /// Размер окна `window` в байтах.
    fn size(&self, window: Window) -> usize;

    /// Читает регистр окна `window` по смещению `offset`.
    fn load(&self, window: Window, offset: usize) -> u32;

    /// Записывает регистр окна `window` по смещению `offset`.
    fn store(&self, window: Window, offset: usize, value: u32);
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
