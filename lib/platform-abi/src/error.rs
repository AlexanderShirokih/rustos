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
