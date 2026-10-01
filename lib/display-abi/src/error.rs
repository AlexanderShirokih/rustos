ipc::wire_enum! {
    /// Отказ операции контракта `Display`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DisplayError {
        /// Сессия уже получила поверхность.
        AlreadyAcquired = 1,
        /// У сессии нет поверхности.
        NoSurface = 2,
        /// Прямоугольник пуст либо выходит за экран.
        OutOfBounds = 3,
        /// Серверу не хватает памяти на поверхность.
        NoSpace = 4,
    }
}
