//! Абстракция транспортного канала.
//!
//! [`Transport`] реализуется платформенным адаптером поверх syscall-ов канала.
//! Кадр кодируется в `bytes`, эндпоинты - в отдельном `handles`-векторе.

use crate::wire::IpcError;

/// Длины прочитанного кадра: байты тела и число хэндлов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageLen {
    /// Число записанных в буфер байт кадра.
    pub bytes: usize,
    /// Число записанных в вектор хэндлов.
    pub handles: usize,
}

impl MessageLen {
    /// Создаёт длины из числа байт и числа хэндлов.
    #[must_use]
    pub const fn new(bytes: usize, handles: usize) -> Self {
        Self { bytes, handles }
    }
}

/// Порт двунаправленного канала сообщений.
/// `bytes` - кадр (заголовок + тело), `handles` - транспортный вектор
/// эндпоинтов кадра.
pub trait Transport {
    /// Отправляет один кадр и его handle-вектор дальнему концу.
    /// `PeerClosed`, если дальний конец закрыт.
    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError>;

    /// Принимает один кадр в `bytes` и его хэндлы в `handles`.
    /// `WouldBlock`, если кадра нет; `PeerClosed` после закрытия и опустошения.
    /// `Truncated`, если буфер короче кадра или хэндлов больше места.
    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError>;

    /// Ждёт готовности к чтению до `timeout_ns` наносекунд.
    /// `Timeout` по истечении срока; `PeerClosed`, если дальний конец закрыт.
    fn wait_readable(&self, timeout_ns: u64) -> Result<(), IpcError>;
}
