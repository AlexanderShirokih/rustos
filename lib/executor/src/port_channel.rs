use ipc::{AsyncChannel, Incoming, Outgoing, Unsent, wire::IpcError};
use syscall::{Handle, SIGNALED, Timeout, WaitItem, kind};
use syscall_user::{Delivery, IssuedHandle, PortTransport, take_call, take_send};

use crate::Reactor;

/// Асинхронный канал IPC-контракта поверх хендла порта. Запускает отложенную операцию порта
/// и ожидает её завершения через [`Reactor`].
///
/// Канал заимствует хендл порта, который остаётся у его владельца. По истечении срока
/// канал закрывает отложенную операцию. Ответ сервера после срока получает `PeerClosed`.
/// Односторонний кадр, не принятый до срока, возвращается без capability: их закрывает ядро
/// вместе с операцией.
pub struct PortChannel {
    transport: PortTransport,
    reactor: Reactor,
}

impl PortChannel {
    /// Канал поверх `port`. Исходы операций задача ждёт через `reactor`.
    #[must_use]
    pub fn new(port: impl Into<Handle<kind::Port>>, reactor: Reactor) -> Self {
        Self {
            transport: PortTransport::new(port),
            reactor,
        }
    }

    async fn settle(
        &self,
        pending: &IssuedHandle<kind::Pending>,
        wait: Timeout,
    ) -> Result<(), IpcError> {
        let signaled = self.reactor.wait(WaitItem::new(pending.as_raw(), SIGNALED));
        match self.reactor.timeout(wait, signaled).await {
            Some(Ok(())) => Ok(()),
            Some(Err(_)) => Err(IpcError::PeerClosed),
            None => Err(IpcError::Timeout),
        }
    }
}

impl AsyncChannel for PortChannel {
    type Resources = Delivery;

    async fn send<'b>(
        &'b self,
        mut frame: Outgoing<'b, Delivery>,
        wait: Timeout,
    ) -> Result<(), Unsent<'b, Delivery>> {
        let pending = match self.transport.post_send(&mut frame) {
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
        let pending = self.transport.post_call(&mut frame)?;
        self.settle(&pending, wait).await?;
        take_call(pending, frame)
    }
}
