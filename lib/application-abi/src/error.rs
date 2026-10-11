ipc::wire_enum! {
    /// Отказ операции контракта `Application`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ApplicationError {
        /// Приложение уже получило окно вызовом `show`.
        Refused = 1,
    }
}
