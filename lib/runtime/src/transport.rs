//! Транспорт ipc-контрактов поверх канальных svc-обёрток.

use ipc::{MessageLen, Transport, wire::IpcError};
use syscall::SYSCALL_RETURN_SHOULD_WAIT;

use crate::{channel_read, channel_write};

/// Порт ipc-канала: оборачивает сырой HandleId эндпоинта.
///
/// Bootstrap-клиенты - отправители (контракт `Bootstrap` несёт только
/// `#[cast]`), поэтому блокирующего приёма транспорт не предоставляет.
#[derive(Clone, Copy)]
pub struct ChannelTransport {
    handle: usize,
}

impl ChannelTransport {
    /// Связывает транспорт с HandleId канального эндпоинта.
    pub fn new(handle: usize) -> Self {
        Self { handle }
    }
}

impl Transport for ChannelTransport {
    fn write_message(&self, bytes: &[u8], _handles: &[u32]) -> Result<(), IpcError> {
        match channel_write(self.handle, bytes) {
            0 => Ok(()),
            SYSCALL_RETURN_SHOULD_WAIT => Err(IpcError::WouldBlock),
            _ => Err(IpcError::PeerClosed),
        }
    }

    fn read_message(&self, bytes: &mut [u8], _handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        let ret = channel_read(self.handle, bytes);
        match u64::try_from(ret) {
            Ok(value) => Ok(MessageLen::new((value & 0xFFFF_FFFF) as usize, 0)),
            Err(_) if ret == SYSCALL_RETURN_SHOULD_WAIT => Err(IpcError::WouldBlock),
            Err(_) => Err(IpcError::PeerClosed),
        }
    }

    /// Sender-side: блокирующего приёма нет (readable-сигнал не зеркалится),
    /// two-way-вызов на этом транспорте вернёт `WouldBlock`.
    fn wait_readable(&self, _timeout_ns: u64) -> Result<(), IpcError> {
        Err(IpcError::WouldBlock)
    }
}
