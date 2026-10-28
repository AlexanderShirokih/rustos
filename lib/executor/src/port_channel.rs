use ipc::{AsyncChannel, Channel, Incoming, Outgoing, Unsent, wire::IpcError};
use kernel_objects::{Session, SessionTransport};
use syscall::{Handle, SIGNALED, Timeout, WaitItem, kind};
use syscall_user::{Delivery, IssuedHandle, PortTransport, take_call, take_send};

use crate::{timeout, wait};

/// Канал IPC-контракта поверх хендла порта. [`Channel`] использует синхронное рандеву,
/// [`AsyncChannel`] запускает отложенную операцию и ждёт её в executor задачи.
///
/// `new` заимствует хендл порта. `client` удерживает сессию до конца операций.
/// По истечении срока канал закрывает отложенную операцию. Ответ сервера после срока
/// получает `PeerClosed`.
/// Односторонний кадр, не принятый до срока, возвращается без capability: их закрывает ядро
/// вместе с операцией.
pub struct PortChannel<T = PortTransport> {
    transport: T,
}

impl PortChannel {
    /// Канал поверх `port`.
    #[must_use]
    pub fn new(port: impl Into<Handle<kind::Port>>) -> Self {
        Self {
            transport: PortTransport::new(port),
        }
    }
}

impl PortChannel<SessionTransport> {
    /// Клиент протокола `P` поверх канала, владеющего сессией `session`. Разрушение канала
    /// закрывает её capability.
    pub fn client<P: ipc::Protocol>(session: Session<P>) -> P::Client<Self> {
        P::client(Self {
            transport: SessionTransport::new(session.into_handle()),
        })
    }
}

impl<T: AsRef<PortTransport>> PortChannel<T> {
    /// Канал поверх существующего транспорта. `transport` задаёт владение сессией.
    pub const fn from_transport(transport: T) -> Self {
        Self { transport }
    }

    async fn settle(
        &self,
        pending: &IssuedHandle<kind::Pending>,
        limit: Timeout,
    ) -> Result<(), IpcError> {
        let signaled = wait(WaitItem::new(pending.as_raw(), SIGNALED));
        match timeout(limit, signaled).await {
            Some(Ok(())) => Ok(()),
            Some(Err(_)) => Err(IpcError::PeerClosed),
            None => Err(IpcError::Timeout),
        }
    }
}

impl<T: AsRef<PortTransport>> Channel for PortChannel<T> {
    type Resources = Delivery;

    fn send<'b>(
        &self,
        frame: Outgoing<'b, Delivery>,
        wait: Timeout,
    ) -> Result<(), Unsent<'b, Delivery>> {
        Channel::send(self.transport.as_ref(), frame, wait)
    }

    fn call<'b>(
        &self,
        frame: Outgoing<'b, Delivery>,
        wait: Timeout,
    ) -> Result<Incoming<'b, Delivery>, IpcError> {
        Channel::call(self.transport.as_ref(), frame, wait)
    }
}

impl<T: AsRef<PortTransport>> AsyncChannel for PortChannel<T> {
    async fn send<'b>(
        &'b self,
        mut frame: Outgoing<'b, Delivery>,
        wait: Timeout,
    ) -> Result<(), Unsent<'b, Delivery>> {
        let pending = match self.transport.as_ref().post_send(&mut frame) {
            Ok(pending) => pending,
            Err(error) => return Err((error, frame)),
        };

        match self.settle(&pending, wait).await {
            Ok(()) => take_send(pending, frame),
            Err(error) => Err((error, frame)),
        }
    }

    async fn call<'b>(
        &'b self,
        mut frame: Outgoing<'b, Delivery>,
        wait: Timeout,
    ) -> Result<Incoming<'b, Delivery>, IpcError> {
        let pending = self.transport.as_ref().post_call(&mut frame)?;
        self.settle(&pending, wait).await?;
        take_call(pending, frame)
    }
}
