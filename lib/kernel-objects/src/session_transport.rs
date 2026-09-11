//! Канал ipc-контрактов поверх порта, capability которого он держит.

use ipc::{
    Channel, FrameBytes, Incoming, Listener, RecvError,
    wire::{IpcError, SessionHandle},
};
use syscall::{Timeout, kind};
use syscall_user::{Delivery, PortTransport};

use crate::handle::OwnedHandle;

/// Канал [`PortTransport`], владеющий capability своего порта.
///
/// Разрушение канала закрывает capability порта. Роль стороны ограничивают права capability:
/// отправка требует `Rights::WRITE`, приём - `Rights::READ`.
pub struct SessionTransport {
    transport: PortTransport,
    _port: OwnedHandle<kind::Port>,
}

impl SessionTransport {
    /// Канал, забирающий порт во владение.
    #[must_use]
    pub fn new(port: OwnedHandle<kind::Port>) -> Self {
        Self {
            transport: PortTransport::new(port.as_raw()),
            _port: port,
        }
    }
}

impl Channel for SessionTransport {
    type Resources = Delivery;

    fn send(&self, bytes: &[u8], handles: &[u32], wait: Timeout) -> Result<(), IpcError> {
        self.transport.send(bytes, handles, wait)
    }

    fn call<'b>(
        &self,
        bytes: &[u8],
        handles: &[u32],
        reply: &'b mut FrameBytes,
        wait: Timeout,
    ) -> Result<Incoming<'b, Delivery>, IpcError> {
        self.transport.call(bytes, handles, reply, wait)
    }
}

impl Listener for SessionTransport {
    type Resources = Delivery;

    fn recv<'b>(
        &self,
        into: &'b mut FrameBytes,
        wait: Timeout,
    ) -> Result<Incoming<'b, Delivery>, RecvError> {
        self.transport.recv(into, wait)
    }
}

impl SessionHandle for OwnedHandle<kind::Port> {
    type Channel = SessionTransport;

    fn into_channel(self) -> SessionTransport {
        SessionTransport::new(self)
    }
}
