//! Владение ресурсами доставленного IPC-кадра.

use ipc::{ReceivedResources, wire::Cap};
use syscall_user::{Delivery, PortTransport};

use crate::handle::OwnedHandle;

/// Владеет capability доставленного кадра и его незавершённым reply.
///
/// Кадр берётся во владение целиком, включая capability сверх тех, что называет его тело.
/// [`claim`](Self::claim) переносит capability потребителю однократно, а `Drop` закрывает
/// невостребованный остаток и завершает reply. Протокол кадра примитиву неизвестен.
pub struct ReceivedFrame<'a> {
    transport: &'a PortTransport,
    delivery: Delivery,
}

impl<'a> ReceivedFrame<'a> {
    /// Берёт доставленный кадр `transport` во владение. `None`, когда доставки не было
    /// либо кадр уже взят.
    pub fn take(transport: &'a PortTransport) -> Option<Self> {
        let delivery = transport.take_delivery()?;
        Some(Self {
            transport,
            delivery,
        })
    }

    /// Значок capability, через которую пришёл кадр. `0` - отправитель слал через
    /// незаклеймённую копию.
    pub fn badge(&self) -> u64 {
        self.delivery.badge
    }

    /// Переносит capability кадра потребителю. `None`, когда кадр её не нёс либо её уже
    /// забрали.
    pub fn claim(&mut self, cap: Cap) -> Option<OwnedHandle> {
        self.delivery.claim(cap).map(OwnedHandle::from)
    }
}

impl Drop for ReceivedFrame<'_> {
    fn drop(&mut self) {
        self.transport.finish_reply(self.delivery.reply);
    }
}
