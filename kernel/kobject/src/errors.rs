/// Ошибки операций над handle'ами и kernel-объектами.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    /// Указан невалидный или уже закрытый handle.
    BadHandle,
    /// Тип объекта не соответствует ожидаемому.
    WrongType,
    /// На handle'е недостаточно прав.
    AccessDenied,
    /// Операция должна быть повторена позже (queue full / no message).
    ShouldWait,
    /// Парный port закрыт.
    PeerClosed,
    /// Истёк deadline.
    Timeout,
    /// Wait отменён: handle, на котором зарегистрировано ожидание, был
    /// закрыт или передан другому процессу до того, как сигнал успел
    /// сработать.
    Canceled,
    /// Буфер получателя меньше длины сообщения.
    BufferTooSmall,
    /// Сообщение превышает лимит размера.
    MessageTooBig,
    /// Handle-таблица процесса исчерпана.
    OutOfHandles,
    /// Бюджет ресурса исчерпан: метерящая операция запросила больше
    /// страниц, чем осталось в [`Resource`](super::Resource).
    ResourceExhausted,
    /// Капа отозвана: один из её предков по цепочке деривации закрыт
    /// (или умерла его таблица). Сам слот ещё может существовать, но
    /// полномочие более не валидно.
    Revoked,
}

/// Ошибки создания процесса/потока через [`KernelRuntime`](super::KernelRuntime).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnError {
    /// Слоты процессов исчерпаны.
    NoFreeProcessSlots,
    /// Слоты потоков исчерпаны.
    NoFreeThreadSlots,
    /// Имя процесса пустое.
    InvalidName,
    /// Приоритет вне допустимого диапазона.
    InvalidPriority,
    /// Размер стека (в страницах) равен нулю.
    InvalidStackPages,
    /// Не удалось аллоцировать стек потока.
    StackAllocationFailed,
    /// Не удалось создать адресное пространство процесса.
    AddressSpaceCreationFailed,
    ImageNotLoaded,
}
