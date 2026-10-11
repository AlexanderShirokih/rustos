ipc::wire_enum! {
    /// Условие срабатывания прерывания устройства.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Trigger {
        /// Подъём уровня.
        Rising = 1,
        /// Спад уровня.
        Falling = 2,
        /// Подъём и спад уровня.
        Both = 3,
        /// Высокий уровень.
        High = 4,
        /// Низкий уровень.
        Low = 5,
    }
}
