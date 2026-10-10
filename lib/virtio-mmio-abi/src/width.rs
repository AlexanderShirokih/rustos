ipc::wire_enum! {
    /// Ширина обращения к полю конфигурации устройства. Дискриминант - число байт.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AccessWidth: u8 {
        /// 1 байт.
        Byte = 1,
        /// 2 байта.
        Half = 2,
        /// 4 байта.
        Word = 4,
    }
}
