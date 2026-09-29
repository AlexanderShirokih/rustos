//! Транспорт кадров поверх рандеву ядра.
//!
//! Клиентская сторона - [`Channel`]: односторонняя отправка и вызов с ответом. Серверная
//! сторона - [`Listener`]: приём кадра вместе с ресурсами его доставки. Ответ встречному вызову
//! уходит каналом [`ReplyChannel`], который приходит в ресурсах кадра вызова. Канал принимает
//! исходящий кадр [`Outgoing`]: байты, заимствованные у построителя
//! [`MessageBuf`](crate::wire::MessageBuf), и владельцев его capability значением.

use core::fmt;

use syscall::Timeout;

use crate::{
    ReceivedResources,
    wire::{Header, IpcError, MESSAGE_INLINE_MAX},
};

/// Буфер приёма одного кадра.
pub type FrameBytes = [u8; MESSAGE_INLINE_MAX];

/// Принятый кадр: его байты в буфере приёма и ресурсы доставки.
pub struct Incoming<'b, R> {
    /// Байты кадра: заголовок и тело.
    pub bytes: &'b [u8],
    /// Ресурсы доставки: capability кадра по слотам и канал ответа встречного вызова.
    pub resources: R,
}

/// Исходящий кадр: байты в буфере построителя [`MessageBuf`](crate::wire::MessageBuf) и
/// ресурсы с владельцами его capability.
///
/// Строит его только [`MessageBuf::outgoing`](crate::wire::MessageBuf::outgoing), и индексы
/// capability в байтах называют слоты тех же ресурсов. Разрушение кадра закрывает capability,
/// которые он держит.
#[derive(Debug)]
pub struct Outgoing<'b, R> {
    buffer: &'b mut FrameBytes,
    len: usize,
    resources: R,
}

impl<'b, R> Outgoing<'b, R> {
    pub(crate) const fn new(buffer: &'b mut FrameBytes, len: usize, resources: R) -> Self {
        Self {
            buffer,
            len,
            resources,
        }
    }

    /// Байты кадра: заголовок и тело.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.buffer.get(..self.len).unwrap_or_default()
    }

    /// Ресурсы кадра: владельцы capability в порядке слотов.
    #[must_use]
    pub const fn resources(&self) -> &R {
        &self.resources
    }

    /// Ресурсы кадра без его байтов.
    #[must_use]
    pub fn into_resources(self) -> R {
        self.resources
    }

    /// Буфер построителя целиком и ресурсы кадра.
    #[must_use]
    pub fn into_parts(self) -> OutgoingParts<'b, R> {
        OutgoingParts {
            buffer: self.buffer,
            resources: self.resources,
        }
    }
}

/// Части исходящего кадра - буфер построителя и ресурсы кадра. Вызов принимает кадр ответа в
/// этот буфер.
#[derive(Debug)]
pub struct OutgoingParts<'b, R> {
    /// Буфер построителя целиком.
    pub buffer: &'b mut FrameBytes,
    /// Ресурсы кадра: владельцы capability в порядке слотов.
    pub resources: R,
}

/// Отказ отправки кадра: причина и кадр вместе с владельцами его capability.
pub type Unsent<'b, R> = (IpcError, Outgoing<'b, R>);

/// Клиентская сторона канала.
pub trait Channel {
    /// Ресурсы исходящего кадра и кадра ответа.
    type Resources: ReceivedResources;

    /// Отправляет односторонний кадр. Успех означает, что получатель принял кадр, и его
    /// capability перенесены. Отказ возвращает кадр вместе с capability. `Timeout` - получатель
    /// не принял кадр за срок `wait`, `PeerClosed` - у канала нет получателя.
    fn send<'b>(
        &self,
        frame: Outgoing<'b, Self::Resources>,
        wait: Timeout,
    ) -> Result<(), Unsent<'b, Self::Resources>>;

    /// Отправляет кадр вызова и принимает кадр ответа этого вызова в буфер кадра вызова. Отказ
    /// разрушает кадр вызова и закрывает его capability, в том числе уже перенесённые серверу.
    /// `Timeout` - ответ не пришёл за срок `wait`, `PeerClosed` - получатель ушёл либо отменил
    /// ответ.
    fn call<'b>(
        &self,
        frame: Outgoing<'b, Self::Resources>,
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
    /// Ресурсы кадра ответа.
    type Resources: ReceivedResources;

    /// Отправляет вызывателю кадр ответа `frame`. `PeerClosed`, если вызыватель больше не ждёт
    /// ответа. Отказ возвращает кадр вместе с capability отправителю.
    fn send(self, frame: Outgoing<'_, Self::Resources>) -> Result<(), Unsent<'_, Self::Resources>>;
}

/// Значение неотправленного кадра вместе с причиной отказа.
///
/// Кадр разобран при отказе тем же декодером, каким его читает получатель, и `Undelivered`
/// держит итог разбора - значение `V` с capability, которые оно называет. Capability кадра, не
/// названные значением, закрыты при разборе, а разрушение `Undelivered` закрывает остальные.
/// Отказ записи кадра закрывает все его capability, и значение заменяет причина отказа.
pub struct Undelivered<V> {
    /// Причина отказа отправки.
    pub error: IpcError,
    value: Result<V, IpcError>,
}

impl<V> Undelivered<V> {
    #[doc(hidden)]
    pub fn __new<R>(
        (error, frame): Unsent<'_, R>,
        read: impl FnOnce(&[u8], &mut R) -> Result<V, IpcError>,
    ) -> Self {
        let Outgoing {
            buffer,
            len,
            mut resources,
        } = frame;
        let value = buffer
            .get(..len)
            .ok_or(IpcError::Truncated)
            .and_then(Header::split)
            .and_then(|parts| read(parts.body, &mut resources));
        Self { error, value }
    }

    pub(crate) fn unwritten(error: IpcError) -> Self {
        Self {
            error,
            value: Err(error),
        }
    }

    /// Значение неотправленного кадра в том виде, в каком его читает получатель. Отказ разбора
    /// кадра приходит как [`IpcError`].
    pub fn into_value(self) -> Result<V, IpcError> {
        self.value
    }
}

impl ReplyChannel for core::convert::Infallible {
    type Resources = ();

    fn send(self, _frame: Outgoing<'_, ()>) -> Result<(), Unsent<'_, ()>> {
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
