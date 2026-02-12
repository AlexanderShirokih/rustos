extern crate alloc;

use crate::{CapabilityError, CapabilityStoreMut};
use alloc::boxed::Box;
use alloc::string::{String, ToString};

/// Дескриптор драйвера, связывающий имя и probe-функцию.
#[repr(C)]
pub struct DriverDescriptor<P> {
    /// Имя драйвера.
    pub name: &'static str,
    /// Функция пробирования.
    pub probe: P,
}

pub trait DriverFactory {
    fn create(&self) -> Result<Box<dyn Driver>, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverRunError {
    MissingCapability { capability: &'static str },
    Fatal(String),
}

impl DriverRunError {
    pub fn from_capability_error(err: CapabilityError) -> Self {
        match err {
            CapabilityError::Missing { type_name } => Self::MissingCapability {
                capability: type_name,
            },
            _ => Self::Fatal(err.to_string()),
        }
    }
}

/// Трейт драйвера устройства
pub trait Driver {
    fn run(&mut self, _caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
        Ok(())
    }
}
