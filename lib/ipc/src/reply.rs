//! Долг ответа вызывателю.

use core::{fmt, marker::PhantomData};

use syscall::kind;

use crate::{
    IntoWireValue, ReceivedResources, ReplyChannel, Undelivered, WireTyped, WireValue,
    schema::WireType,
    wire::{Cap, FIELD_ID_MIN, Header, IpcError, MessageBuf},
};

crate::wire_value! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct Record<C = Cap<kind::Reply>> {
        channel: C,
        ordinal: u64,
    }
}

/// Долг ответа на вызов: канал ответа вызывателю и ординал его операции.
///
/// `V` - тип ответа, `C` - канал. Долг отвечают один раз вызовом `send`, в том числе после
/// возврата обработчика и из другого потока. Разрушение долга без ответа отменяет ответ, и
/// вызыватель получает `PeerClosed`. Долг передаётся другому процессу владеющим значением поля
/// типа `Reply<V>`, и получатель читает его долгом со своим каналом ответа. Ответ по
/// пересланному долгу содержит ординал исходной операции.
#[must_use = "a dropped reply refuses its caller"]
pub struct Reply<V, C = Cap<kind::Reply>> {
    record: Record<C>,
    answer: PhantomData<fn() -> V>,
}

impl<V, C> Reply<V, C> {
    #[doc(hidden)]
    pub const fn __new(channel: C, ordinal: u64) -> Self {
        Self {
            record: Record { channel, ordinal },
            answer: PhantomData,
        }
    }
}

impl<V, C: ReplyChannel> Reply<V, C> {
    /// Отвечает вызывателю значением `value`. Долг расходуется на любом исходе. Успех отдаёт
    /// capability значения вызывателю, отказ возвращает значение отправителю вместе с причиной.
    pub fn send<W: IntoWireValue<Read = V>>(self, value: W) -> Result<(), Undelivered<W>> {
        let Record { channel, ordinal } = self.record;
        let header = Header::new(ordinal);
        let sent = crate::protocol::send_frame(
            header,
            |frame| value.write_as_field(frame, FIELD_ID_MIN),
            |bytes, handles| channel.send(bytes, handles),
        );
        match sent {
            Ok(()) => {
                value.relinquish();
                Ok(())
            }
            Err(error) => Err(Undelivered { error, value }),
        }
    }
}

impl<V, C: IntoWireValue<Read = Cap<kind::Reply>>> IntoWireValue for Reply<V, C> {
    type Read = Reply<V>;

    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
        self.record.write_as_field(buf, field_id)
    }

    fn relinquish(self) {
        self.record.relinquish();
    }
}

/// Долг читается владеющим каналом ответа, который пришёл capability поля.
impl<'a, V, R: ReceivedResources> WireValue<'a, R> for Reply<V> {
    type Owned = Reply<V, R::Reply>;

    fn from_field(data: &'a [u8], frame: &mut R) -> Result<Self::Owned, IpcError> {
        let Record { channel, ordinal } = <Record as WireValue<'a, R>>::from_field(data, frame)?;
        let channel = R::reply_channel(channel).ok_or(IpcError::InvalidValue)?;
        Ok(Reply::__new(channel, ordinal))
    }
}

impl<V> WireTyped for Reply<V> {
    const WIRE_TYPE: WireType = <Record as WireTyped>::WIRE_TYPE;
}

impl<V, C: fmt::Debug> fmt::Debug for Reply<V, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Reply").field(&self.record).finish()
    }
}

#[cfg(test)]
mod tests {
    use syscall::{Handle, kind};

    use super::Reply;
    use crate::{
        IntoWireValue, WireValue,
        resources::testing::{TestFrame, TestReply},
        wire::{Cap, FieldCursor, IpcError, MessageBuf},
    };

    #[test]
    fn a_debt_arrives_owning_the_channel_of_its_field() {
        let channel = Cap::<kind::Reply>::from_handle(Handle::from_abi(
            core::num::NonZeroU32::new(0x2B).expect("non-zero"),
        ));
        let mut buf = MessageBuf::new();
        Reply::<u32>::__new(channel, 0x0102_0304_0506_0708)
            .write_as_field(&mut buf, 1)
            .expect("write ok");
        buf.finish().expect("finish ok");

        let field = FieldCursor::new(buf.as_bytes())
            .next_field()
            .expect("ok")
            .expect("present");
        let debt = <Reply<u32> as WireValue<TestFrame>>::from_field(
            field.data,
            &mut TestFrame::of(buf.handles()),
        )
        .expect("the debt is read");
        assert_eq!(debt.record.channel, TestReply(0x2B));
        assert_eq!(debt.record.ordinal, 0x0102_0304_0506_0708);
    }

    #[test]
    fn a_debt_record_without_its_ordinal_is_refused() {
        let mut record = MessageBuf::new();
        record.write_field(1, &[0]).expect("channel ok");
        record.finish().expect("finish ok");

        assert_eq!(
            <Reply<u32> as WireValue<TestFrame>>::from_field(
                record.as_bytes(),
                &mut TestFrame::of(&[0x2B])
            )
            .map(drop),
            Err(IpcError::MissingField)
        );
    }
}
