use platform_abi::DEVICE_ID_MAX;
use syscall::kind;

use crate::{
    dimension::{Area, DisplayDimension},
    error::DisplayError,
};

ipc::protocol! {
    /// Экран, к которому открыта сессия.
    pub mod display {
        name = "Display";

        /// Размер экрана в пикселях.
        call fn dimension() -> DisplayDimension;

        /// Выдаёт сессии RW-capability на регион поверхности размером не меньше
        /// [`DisplayDimension::surface_size`] байт. Повторный вызов в той же сессии возвращает
        /// `AlreadyAcquired`, нехватка памяти у сервера - `NoSpace`.
        ///
        /// Регион принадлежит серверу. Закрытие сессии отзывает выданную capability. Отозванная
        /// capability удерживает страницы региона, пока клиент её не закроет.
        call fn acquire_surface() -> own Result<ipc::wire::Cap<kind::MemoryRegion>, DisplayError>;

        /// Копирует пиксели поверхности внутри `area` в буфер экрана и отвечает после записи.
        /// Возвращает `NoSurface`, если у сессии нет поверхности, и `OutOfBounds` для `area`,
        /// которую отвергает [`DisplayDimension::check_area`]. При отказе экран не меняется.
        call fn present(area: Area) -> Result<(), DisplayError>;
    }
}

ipc::protocol! {
    /// Открытие сессий [`display`] к экранам драйвера.
    pub mod display_provider {
        name = "DisplayProvider";

        /// Открывает сессию [`display`] к экрану `device`. `device` - путь
        /// [`DeviceId`](platform_abi::DeviceId) устройства в описании платформы.
        ///
        /// Если описание не содержит устройства `device` или драйвер его не поддерживает,
        /// возвращает `Refused`. Если драйверу не хватает ресурсов на сессию, возвращает
        /// `NoSpace`.
        call fn open(
            device: ipc::wire::Str<{ DEVICE_ID_MAX }>,
        ) -> own Result<ipc::wire::Session<crate::display::Protocol>, DisplayError>;
    }
}
