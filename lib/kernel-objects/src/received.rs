//! Владение ресурсами доставленного IPC-кадра.

use ipc::{ReceivedResources, wire::Cap};
use syscall::{ObjectKind, kind};
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
    /// забрали. Вид владельца приходит от вида поля контракта.
    pub fn claim<K: ObjectKind>(&mut self, cap: Cap<K>) -> Option<OwnedHandle<K>> {
        self.delivery.claim(cap).map(OwnedHandle::from)
    }
}

impl Drop for ReceivedFrame<'_> {
    fn drop(&mut self) {
        self.transport.finish_reply(self.delivery.reply);
    }
}

/// Укладывает хэндл в кадр ответа, оставляя владение за отвечающим до конца кадра. Дальше хэндл
/// принимает [`settle`]. Кадр отвечает одной capability, и повторный вызов по тому же `pending`
/// закрывает прежнюю. Токен ответа содержит вид выданного хэндла, тогда как владелец лежит
/// в гетерогенной ячейке:
///
/// ```no_run
/// use core::num::NonZeroU32;
/// use ipc::wire::Cap;
/// use kernel_objects::{Port, kind, vend};
/// use syscall::Handle;
///
/// let mut pending = None;
/// let port = Port::from_handle(Handle::from_abi(NonZeroU32::new(4).unwrap()));
/// let token: Cap<kind::Port> = vend(&mut pending, port);
/// assert_eq!(token.raw().get(), 4);
/// ```
pub fn vend<K: ObjectKind>(
    pending: &mut Option<OwnedHandle<kind::Any>>,
    handle: OwnedHandle<K>,
) -> Cap<K> {
    let token = Cap::from_handle(handle.as_raw());
    *pending = Some(handle.erase());
    token
}

/// Закрывает выданную capability, когда ответ не доставлен. Доставленный ответ переносит её
/// получателю ядром, и сервер отпускает свой хэндл без закрытия. Вызов идёт один раз на кадр,
/// после обработки.
pub fn settle(pending: &mut Option<OwnedHandle<kind::Any>>, delivered: bool) {
    let Some(handle) = pending.take() else {
        return;
    };
    if delivered {
        let _ = handle.into_raw();
    } else {
        let _ = handle.close();
    }
}
