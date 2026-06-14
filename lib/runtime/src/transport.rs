//! Транспорт ipc-контрактов поверх канальных svc-обёрток.

use ipc::{MessageLen, Transport, wire::IpcError, wire::MESSAGE_MAX_HANDLES};
use syscall::{Handle, SYSCALL_RETURN_SHOULD_WAIT};

use crate::{channel_read, channel_write};

/// Порт ipc-канала: оборачивает handle эндпоинта.
///
/// Bootstrap-клиенты - отправители (контракт `Bootstrap` несёт только
/// `#[cast]`), поэтому блокирующего приёма транспорт не предоставляет.
#[derive(Clone, Copy)]
pub struct ChannelTransport {
    handle: Handle,
}

impl ChannelTransport {
    /// Связывает транспорт с handle канального эндпоинта.
    pub fn new(handle: Handle) -> Self {
        Self { handle }
    }
}

impl Transport for ChannelTransport {
    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        // Транспорт говорит на wire-`u32`, svc-обёртка - на типизированном
        // `Handle`; поднимаем сырые HandleId в `Handle` (нулевой - битый кадр).
        if handles.len() > MESSAGE_MAX_HANDLES {
            return Err(IpcError::BoundExceeded);
        }
        let mut typed = [self.handle; MESSAGE_MAX_HANDLES];
        for (slot, &raw) in typed.iter_mut().zip(handles) {
            *slot = Handle::new(raw).ok_or(IpcError::BoundExceeded)?;
        }

        match channel_write(self.handle, bytes, &typed[..handles.len()]) {
            0 => Ok(()),
            SYSCALL_RETURN_SHOULD_WAIT => Err(IpcError::WouldBlock),
            _ => Err(IpcError::PeerClosed),
        }
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        let mut typed = [None; MESSAGE_MAX_HANDLES];
        let cap = handles.len().min(MESSAGE_MAX_HANDLES);
        let ret = channel_read(self.handle, bytes, &mut typed[..cap]);
        match u64::try_from(ret) {
            Ok(value) => {
                let count = (value >> 32) as usize;
                for (dst, slot) in handles.iter_mut().zip(&typed[..count]) {
                    *dst = slot.map_or(0, Handle::raw);
                }
                Ok(MessageLen::new((value & 0xFFFF_FFFF) as usize, count))
            }
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
