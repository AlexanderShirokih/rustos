extern crate alloc;

use alloc::{
    boxed::Box,
    string::{String, ToString},
};

use crate::{BootServices, BootServicesError, ServiceKind};

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
    MissingService { service: ServiceKind },
    Fatal(String),
}

impl DriverRunError {
    pub fn from_boot_services_error(err: BootServicesError) -> Self {
        match err {
            BootServicesError::Missing(service) => Self::MissingService { service },
            BootServicesError::Duplicate(_) => Self::Fatal(err.to_string()),
        }
    }
}

/// Трейт драйвера устройства
pub trait Driver {
    fn run(&mut self, _services: &mut BootServices) -> Result<(), DriverRunError> {
        Ok(())
    }
}
