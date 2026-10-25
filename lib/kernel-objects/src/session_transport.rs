//! Канал ipc-контрактов поверх порта, capability которого он держит.

use ipc::{
    Channel, FrameBytes, Incoming, Listener, Outgoing, RecvError, Unsent,
    wire::{IpcError, SessionHandle},
};
use syscall::Timeout;
use syscall_user::{Delivery, PortTransport};

use crate::Port;

/// Канал [`PortTransport`], владеющий capability своего порта.
///
/// Разрушение канала закрывает capability порта. Роль стороны ограничивают права capability:
/// отправка требует `Rights::WRITE`, приём - `Rights::READ`.
pub struct SessionTransport {
    transport: PortTransport,
    _port: Port,
}

impl SessionTransport {
    /// Канал, забирающий порт во владение.
    #[must_use]
    pub fn new(port: Port) -> Self {
        Self {
            transport: PortTransport::new(port.as_raw()),
            _port: port,
        }
    }
}

impl AsRef<PortTransport> for SessionTransport {
    fn as_ref(&self) -> &PortTransport {
        &self.transport
    }
}

impl Channel for SessionTransport {
    type Resources = Delivery;

    fn send<'b>(
        &self,
        frame: Outgoing<'b, Delivery>,
        wait: Timeout,
    ) -> Result<(), Unsent<'b, Delivery>> {
        self.transport.send(frame, wait)
    }

    fn call<'b>(
        &self,
        frame: Outgoing<'b, Delivery>,
        wait: Timeout,
    ) -> Result<Incoming<'b, Delivery>, IpcError> {
        self.transport.call(frame, wait)
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

impl SessionHandle for Port {
    type Channel = SessionTransport;

    fn into_channel(self) -> SessionTransport {
        SessionTransport::new(self)
    }
}
