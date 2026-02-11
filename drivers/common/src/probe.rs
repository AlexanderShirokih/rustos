use crate::driver::DriverFactory;
use alloc::boxed::Box;
use alloc::string::String;
use core::fmt;

/// Результат пробирования драйвера.
pub type ProbeResult = Result<Box<dyn DriverFactory>, ProbeError>;

/// Ошибки при пробировании устройства.
#[derive(Debug)]
pub enum ProbeError {
    DriverCreationFailed(String),

    /// Отсутствует обязательное свойство.
    MissingProperty(&'static str),
    /// Устройство или функция не поддерживается.
    Unsupported(&'static str),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeError::DriverCreationFailed(msg) => write!(f, "Failed to create driver: {}", msg),
            ProbeError::MissingProperty(prop) => write!(f, "Missing property '{}'", prop),
            ProbeError::Unsupported(feature) => write!(f, "Unsupported: {}", feature),
        }
    }
}
