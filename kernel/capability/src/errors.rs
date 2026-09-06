/// Ошибки операций над handle и kernel-объектами.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum IpcError {
    /// Указан невалидный или уже закрытый handle.
    BadHandle,
    /// Тип объекта не соответствует ожидаемому.
    WrongType,
    /// На handle недостаточно прав.
    AccessDenied,
    /// Операция должна быть повторена позже (queue full / no message).
    ShouldWait,
    /// Парный port закрыт.
    PeerClosed,
    /// Отправитель значка исчез. Кадр отсутствует, а значок доставлен в `IpcBuffer.badge`
    /// получателя.
    SenderGone,
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
    /// Capability-таблица процесса исчерпана.
    OutOfHandles,
    /// Бюджет ресурса исчерпан: метерящая операция запросила больше
    /// страниц, чем осталось в [`Resource`](super::Resource).
    ResourceExhausted,
    /// Капа отозвана: один из её предков по цепочке деривации закрыт
    /// (или его таблица уничтожена). Сам слот ещё может существовать, но
    /// полномочие более не валидно.
    Revoked,
    /// Линия прерывания уже привязана. Разделяемых линий в проекте не существует. Повторный
    /// минт занятой линии отвергается.
    AlreadyBound,
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
    /// Поток создаётся в процессе, образ которого ещё не загружен.
    ImageNotLoaded,
    /// Процесс уже завершён.
    ProcessTerminated,
}
