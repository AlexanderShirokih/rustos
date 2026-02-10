//! Контракты подсистемы прерываний.

#![no_std]

/// Номер аппаратного прерывания.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct IrqNumber(u16);

impl IrqNumber {
    /// Максимальное валидное значение IRQ для базовой конфигурации GICv2.
    pub const MAX: u16 = 1023;

    /// Создаёт номер прерывания, если значение валидно.
    pub const fn new(raw: u16) -> Option<Self> {
        if raw <= Self::MAX {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// Возвращает сырое значение IRQ.
    pub const fn raw(self) -> u16 {
        self.0
    }
}

/// Дескриптор зарегистрированного обработчика.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct IrqRegistrationToken(pub u32);

/// Ошибки регистрации обработчика IRQ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqRegistrationError {
    /// Некорректный номер IRQ.
    InvalidIrq,
    /// Обработчик для данного IRQ уже существует.
    AlreadyRegistered,
    /// Регистрация пока не поддерживается текущим окружением.
    Unsupported,
    /// Прочая ошибка с сообщением.
    Other(&'static str),
}

/// Контракт обработчика IRQ.
///
/// Трейт object-safe, чтобы драйвер мог передавать stateful обработчик.
pub trait IrqHandler: Sync {
    /// Вызывается при срабатывании соответствующего IRQ.
    fn handle(&self, irq: IrqNumber);
}
