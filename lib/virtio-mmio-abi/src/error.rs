ipc::wire_enum! {
    /// Отказ операции контракта `VirtioMmio`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum VirtioMmioError {
        /// Ни у одного транспорта нет устройства запрошенного типа.
        NoDevice = 1,
        /// Каждый транспорт с устройством запрошенного типа занят.
        Busy = 2,
        /// Шине не хватает ресурса на сессию слота.
        NoSpace = 3,
    }
}

ipc::wire_enum! {
    /// Отказ операции контракта `VirtioSlot`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SlotError {
        /// Смещение регистра выходит за окно транспорта либо не кратно 4.
        OutOfRange = 1,
        /// Шина не смогла выдать ресурс транспорта.
        Unavailable = 2,
    }
}
