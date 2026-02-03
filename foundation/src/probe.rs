//! Базовые типы для пробирования устройств.

use core::fmt;

/// Результат пробирования драйвера.
pub type ProbeResult<T> = Result<T, ProbeError>;

/// Адрес MMIO-региона.
pub type MmioAddress = usize;

/// Запрос на маппинг MMIO-региона от драйвера.
#[derive(Debug, Clone, Copy)]
pub struct MmioRequest {
    /// Базовый адрес региона.
    pub base: MmioAddress,
    /// Размер региона в байтах.
    pub size: usize,
}

/// Ошибки при пробировании устройства.
#[derive(Debug)]
pub enum ProbeError {
    /// Отсутствует обязательное свойство.
    MissingProperty(&'static str),
    /// Устройство или функция не поддерживается.
    Unsupported(&'static str),
    /// Прочая ошибка.
    Other(&'static str),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeError::MissingProperty(prop) => write!(f, "missing property '{}'", prop),
            ProbeError::Unsupported(feature) => write!(f, "unsupported: {}", feature),
            ProbeError::Other(msg) => f.write_str(msg),
        }
    }
}
