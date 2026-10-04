ipc::wire_enum! {
    /// Отказ в линии либо окне устройства.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PlatformError {
        /// В описании платформы нет устройства, линии либо окна.
        NotDescribed = 1,
        /// Линию либо регион окна держит другая сессия.
        Taken = 2,
        /// Ресурс описан, но не выдан.
        Unavailable = 3,
    }
}

/// Отказ вида описания платформы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptionError {
    /// Описание выходит за регион, его заголовок неверен либо структура повреждена.
    Malformed,
    /// Значение свойства не соответствует запрошенному виду.
    InvalidValue,
    /// Путь доступного узла длиннее [`DEVICE_ID_MAX`](crate::DEVICE_ID_MAX) байт либо
    /// [`BUS_PATH_MAX_NODES`](fdt::devicetreeext::BUS_PATH_MAX_NODES) узлов.
    PathTooLong,
}
