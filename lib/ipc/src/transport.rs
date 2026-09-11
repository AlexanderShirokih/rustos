//! Транспорт кадров поверх рандеву ядра.
//!
//! Клиентская сторона - [`Channel`]: односторонняя отправка и вызов с ответом. Серверная
//! сторона - [`Listener`]: приём кадра вместе с ресурсами его доставки. Ответ встречному вызову
//! уходит каналом [`ReplyChannel`], который приходит в ресурсах кадра вызова. Кадр кодируется в
//! `bytes`, capability - в отдельном `handles`-векторе.

use core::fmt;

use syscall::Timeout;

use crate::wire::{IpcError, MESSAGE_INLINE_MAX};

/// Буфер приёма одного кадра.
pub type FrameBytes = [u8; MESSAGE_INLINE_MAX];

/// Принятый кадр: его байты в буфере приёма и ресурсы доставки.
pub struct Incoming<'b, R> {
    /// Байты кадра: заголовок и тело.
    pub bytes: &'b [u8],
    /// Ресурсы доставки: capability кадра по слотам и канал ответа встречного вызова.
    pub resources: R,
}

/// Клиентская сторона канала.
pub trait Channel {
    /// Ресурсы кадра ответа.
    type Resources: crate::ReceivedResources;

    /// Отправляет односторонний кадр. Успех означает, что получатель принял кадр, и его
    /// capability перенесены. `Timeout` - получатель не принял кадр за срок `wait`, и capability
    /// остались у отправителя. `PeerClosed` - у канала нет получателя.
    fn send(&self, bytes: &[u8], handles: &[u32], wait: Timeout) -> Result<(), IpcError>;

    /// Отправляет кадр вызова и принимает в `reply` кадр ответа этого вызова. `Timeout` -
    /// ответ не пришёл за срок `wait`, `PeerClosed` - получатель ушёл либо отменил ответ.
    fn call<'b>(
        &self,
        bytes: &[u8],
        handles: &[u32],
        reply: &'b mut FrameBytes,
        wait: Timeout,
    ) -> Result<Incoming<'b, Self::Resources>, IpcError>;
}

/// Серверная сторона канала.
pub trait Listener {
    /// Ресурсы принятого кадра.
    type Resources: crate::ReceivedResources;

    /// Принимает в `into` один кадр в пределах срока `wait`.
    fn recv<'b>(
        &self,
        into: &'b mut FrameBytes,
        wait: Timeout,
    ) -> Result<Incoming<'b, Self::Resources>, RecvError>;
}

/// Отказ приёма кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    /// Вместо кадра доставлено событие ухода отправителей значка `badge`.
    SenderGone(u64),
    /// Истёк срок ожидания.
    Timeout,
    /// Кадр доставлен, а принять его нечем. Кадр отброшен, и его вызыватель получил отказ.
    Discarded,
    /// Канал закрыт, и кадров больше не будет.
    Closed,
}

/// Отказ приёма как отказ операции, которой кадр нужен сейчас. Уход отправителя и закрытие
/// канала для неё равны `PeerClosed`, а цикл приёма, для которого уход значка не конец,
/// разбирает [`RecvError`] сам.
impl From<RecvError> for IpcError {
    fn from(error: RecvError) -> Self {
        match error {
            RecvError::Timeout => Self::Timeout,
            RecvError::Discarded => Self::ResourceExhausted,
            RecvError::SenderGone(_) | RecvError::Closed => Self::PeerClosed,
        }
    }
}

/// Канал одного ответа вызывателю, пришедший с кадром его вызова.
///
/// Канал отправляет единственный кадр ответа и исчерпывается на любом исходе отправки.
/// Разрушение канала без отправки отменяет ответ, и вызыватель получает `PeerClosed`.
pub trait ReplyChannel: Sized {
    /// Отправляет вызывателю кадр `bytes` и его handle-вектор `handles`. `PeerClosed`, если
    /// вызыватель больше не ждёт ответа. При отказе capability `handles` остаются у
    /// отправителя.
    fn send(self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError>;
}

/// Неотправленное значение вместе с причиной отказа. Capability значения остались у
/// отправителя.
pub struct Undelivered<V> {
    /// Причина отказа отправки.
    pub error: IpcError,
    /// Значение, которое отправка не передала.
    pub value: V,
}

impl ReplyChannel for core::convert::Infallible {
    fn send(self, _bytes: &[u8], _handles: &[u32]) -> Result<(), IpcError> {
        match self {}
    }
}

impl<V> fmt::Debug for Undelivered<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Undelivered")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl<V> PartialEq for Undelivered<V> {
    fn eq(&self, other: &Self) -> bool {
        self.error == other.error
    }
}

impl<V> From<Undelivered<V>> for IpcError {
    fn from(undelivered: Undelivered<V>) -> Self {
        undelivered.error
    }
}
