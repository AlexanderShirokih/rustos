extern crate alloc;

use alloc::{
    boxed::Box,
    string::{String, ToString},
};

use crate::{BootServices, BootServicesError, ServiceKind};

/// Дескриптор драйвера: имя записи и функция пробирования узла на устройство.
#[repr(C)]
pub struct DriverDescriptor<P> {
    /// Имя записи в реестре поднятых драйверов и в логе инициализации.
    pub name: &'static str,
    /// Пробует узел и возвращает фабрику драйвера при совпадении, ошибку - при отказе.
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

/// Реализация настраивает устройство и публикует свой сервис в [`BootServices`].
///
/// `kernelspace` вызывает `run` на каждом проходе подъёма. Реализация проверяет нужные сервисы
/// через `require_*` раньше первого обращения к железу.
///
/// Результат `run` решает дальнейшее действие `kernelspace`:
/// - `Ok(())` - `kernelspace` больше не вызывает `run` для этого драйвера;
/// - `DriverRunError::MissingService` - `kernelspace` вызывает `run` того же экземпляра снова
///   на следующем проходе;
/// - `DriverRunError::Fatal` - `kernelspace` останавливает подъём целиком.
pub trait Driver {
    fn run(&mut self, _services: &mut BootServices) -> Result<(), DriverRunError> {
        Ok(())
    }
}
