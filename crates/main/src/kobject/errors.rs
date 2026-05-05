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
    /// Парный endpoint закрыт.
    PeerClosed,
    /// Истёк deadline.
    Timeout,
    /// Буфер получателя меньше длины сообщения.
    BufferTooSmall,
    /// Сообщение превышает лимит размера.
    MessageTooBig,
    /// Handle-таблица процесса исчерпана.
    OutOfHandles,
}
