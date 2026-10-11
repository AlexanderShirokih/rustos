//! Номера, которые выдаёт хаб ввода.

use core::{
    fmt,
    num::{NonZeroU16, NonZeroU32},
};

ipc::wire_value! {
    /// Номер устройства. Хаб выдаёт номера по возрастанию с [`DeviceNumber::FIRST`] и не
    /// повторяет их.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct DeviceNumber(NonZeroU16);
}

ipc::wire_value! {
    /// Номер сессии фокуса. Хаб выдаёт номера по возрастанию с [`FocusNumber::FIRST`] и не
    /// повторяет их.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct FocusNumber(NonZeroU32);
}

impl DeviceNumber {
    /// Первый выдаваемый номер.
    pub const FIRST: Self = Self(NonZeroU16::MIN);

    /// Номер из значения поля записи. Нуль означает отсутствие устройства.
    #[must_use]
    pub const fn new(raw: u16) -> Option<Self> {
        match NonZeroU16::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    /// Значение номера для поля записи.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }

    /// Следующий номер либо `None` по исчерпании номеров.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }
}

impl fmt::Display for DeviceNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FocusNumber {
    /// Первый выдаваемый номер.
    pub const FIRST: Self = Self(NonZeroU32::MIN);

    /// Следующий номер либо `None` по исчерпании номеров.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }
}
