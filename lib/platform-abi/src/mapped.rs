use ipc::wire::IpcError;
use kernel_objects::{MapError, MemoryRegion, MmioMapping, MmioWindow, ReadOnlyMapping};

use crate::{
    description::{Description, DescriptionRegion},
    device::DeviceId,
    error::{DescriptionError, PlatformError},
    protocol::platform,
};

/// Выданное описание платформы, отображённое только для чтения. Держит регион описания
/// отображением и выдаёт [`Description`] над его байтами.
#[derive(Debug)]
pub struct MappedDescription {
    mapping: ReadOnlyMapping,
    offset: u64,
}

/// Отказ [`platform::Client::map_mmio`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowRefusal {
    /// Вызов `mmio` не дошёл до `Platform`.
    Call(IpcError),
    /// `Platform` отказал в окне.
    Platform(PlatformError),
    /// Окно не отобразилось.
    Map(kernel_objects::Error),
}

impl MappedDescription {
    /// Отображает регион описания `description` и забирает его. Отказ отображения возвращает
    /// регион в [`MapError`], как [`ReadOnlyMapping::new`].
    pub fn new<M: Into<MemoryRegion>>(
        description: DescriptionRegion<M>,
    ) -> Result<Self, MapError<MemoryRegion>> {
        let mapping = ReadOnlyMapping::new(description.region.into())?;
        Ok(Self {
            mapping,
            offset: description.offset,
        })
    }

    /// Разбирает описание при каждом вызове, как [`Description::new`].
    pub fn description(&self) -> Result<Description<'_>, DescriptionError> {
        Description::new(self.mapping.as_bytes(), self.offset)
    }
}

impl<C: ipc::AsyncChannel<Resources = kernel_objects::Delivery>> platform::Client<C> {
    /// Берёт и отображает окно устройства. Ответ `Platform` задача ждёт через канал клиента.
    /// Окно отзывается с концом сессии, которая его выдала.
    pub async fn map_mmio(
        &self,
        device: &DeviceId,
        resource: u16,
    ) -> Result<MmioMapping, WindowRefusal> {
        let window = self
            .mmio(device.clone(), resource)
            .await
            .map_err(|error| error.fold(WindowRefusal::Call, WindowRefusal::Platform))?;

        MmioMapping::new(MmioWindow::from(window))
            .map_err(|refused| WindowRefusal::Map(refused.error))
    }
}
