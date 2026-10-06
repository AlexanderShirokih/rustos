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
