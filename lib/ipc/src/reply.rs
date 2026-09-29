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
    /// capability значения вызывателю, отказ возвращает значение в том виде, в каком его читает
    /// вызыватель, вместе с причиной. Отказ записи кадра закрывает capability значения.
    pub fn send<W, O>(self, value: W) -> Result<(), Undelivered<O>>
    where
        W: IntoWireValue<C::Resources, Read = V>,
        V: for<'a> WireValue<'a, C::Resources, Owned = O>,
    {
        let Record { channel, ordinal } = self.record;
        let mut frame = MessageBuf::new();
        crate::protocol::build_frame(&mut frame, Header::new(ordinal), |frame| {
            value.write_into(frame, FIELD_ID_MIN)
        })
        .map_err(Undelivered::unwritten)?;
        channel.send(frame.outgoing()).map_err(|unsent| {
            Undelivered::__new(unsent, |body, resources| {
                crate::protocol::read_answer::<V, _>(body, resources)
            })
        })
    }
}

impl<V, R: ReceivedResources, C: IntoWireValue<R, Read = Cap<kind::Reply>>> IntoWireValue<R>
    for Reply<V, C>
{
    type Read = Reply<V>;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        self.record.write_into(frame, field_id)
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
    use syscall::kind;

    use super::Reply;
    use crate::{
        IntoWireValue, WireValue,
        resources::testing::{TestFrame, TestReply, cap},
        wire::{FieldCursor, IpcError, MessageBuf},
    };

    #[test]
    fn a_debt_arrives_owning_the_channel_of_its_field() {
        let mut buf = MessageBuf::<TestFrame>::new();
        Reply::<u32, _>::__new(TestReply(0x2B), 0x0102_0304_0506_0708)
            .write_into(&mut buf, 1)
            .expect("write ok");
        buf.finish().expect("finish ok");

        let bytes = buf.as_bytes().to_vec();
        let field = FieldCursor::new(&bytes)
            .next_field()
            .expect("ok")
            .expect("present");
        let debt =
            <Reply<u32> as WireValue<TestFrame>>::from_field(field.data, &mut buf.into_resources())
                .expect("the debt is read");
        assert_eq!(debt.record.channel, TestReply(0x2B));
        assert_eq!(debt.record.ordinal, 0x0102_0304_0506_0708);
    }

    #[test]
    fn a_debt_record_without_its_ordinal_is_refused() {
        let mut record = MessageBuf::<TestFrame>::new();
        record
            .write_handle(1, cap::<kind::Reply>(0x2B))
            .expect("channel ok");
        record.finish().expect("finish ok");

        let bytes = record.as_bytes().to_vec();
        assert_eq!(
            <Reply<u32> as WireValue<TestFrame>>::from_field(&bytes, &mut record.into_resources())
                .map(drop),
            Err(IpcError::MissingField)
        );
    }

    #[test]
    fn a_refused_answer_comes_back_as_its_caller_reads_it() {
        let refused = Reply::<crate::wire::Cap<kind::Port>, _>::__new(TestReply(0x2B), 7)
            .send(cap::<kind::Port>(0x2A))
            .expect_err("the caller is gone");
        assert_eq!(refused.error, IpcError::PeerClosed);
        assert_eq!(refused.into_value(), Ok(cap(0x2A)));
    }
}
