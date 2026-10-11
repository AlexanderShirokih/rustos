//! Кодек записи поля: чтение [`WireValue`], запись [`IntoWireValue`] и дескриптор схемы
//! [`WireTyped`].

use core::num::{NonZeroU16, NonZeroU32};

use syscall::ObjectKind;

use crate::{
    ReceivedResources,
    schema::{self, WireType},
    wire::{Cap, Field, FieldCursor, IpcError, MessageBuf, value},
};

pub(crate) const RESULT_OK_FIELD: u8 = 1;
const RESULT_ERR_FIELD: u8 = 2;

/// Тип записи поля, которую получатель читает из кадра с ресурсами `R`.
///
/// Разбор забирает из ресурсов кадра capability, которые называет запись, и прочитанное значение
/// `Owned` ими владеет. У типа без capability `Owned` совпадает с самим типом.
/// Заимствование из `data` имеет lifetime `'a`.
pub trait WireValue<'a, R: ReceivedResources>: Sized {
    /// Значение, которое получатель читает из записи этого типа.
    type Owned;

    /// Читает значение из `data` записи поля. Capability поля забираются из `frame`.
    ///
    /// Реализация строит владеющее значение сразу в возвращаемом `Result`, без промежуточных
    /// `Option` и `Result`.
    fn from_field(data: &'a [u8], frame: &mut R) -> Result<Self::Owned, IpcError>;
}

/// Значение исходящего кадра с ресурсами `R`, которое пишет одну запись поля.
///
/// Запись забирает значение, и владельцы его capability переходят в ресурсы кадра. Успех
/// отправки отдаёт capability получателю. Отказ отправки кадра возвращает значение в том виде, в
/// каком его читает получатель, а разрушение кадра закрывает capability. Значение без capability
/// пишется в кадр с любыми ресурсами.
pub trait IntoWireValue<R: ReceivedResources>: Sized {
    /// Тип, которым получатель читает эту запись.
    type Read;

    /// Пишет запись поля `field_id` в кадр `frame`. Владельцы capability значения занимают
    /// слоты ресурсов кадра.
    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError>;
}

/// Структурный wire-дескриптор типа записи. Его читают const-проверки объявления, которые
/// сверяют бюджет кадра, пространство `field_id` и отсутствие capability у параметра без
/// `own`. Трейт не зависит от лайфтайма. Агрегат собирает дескриптор из полей.
pub trait WireTyped {
    /// Дескриптор раскладки значения в сообщении.
    const WIRE_TYPE: WireType;
}

macro_rules! int_wire_value {
    ($ty:ty, $encode:ident, $decode:ident, $desc:expr) => {
        impl<'a, R: ReceivedResources> WireValue<'a, R> for $ty {
            type Owned = Self;

            fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
                value::$decode(data)
            }
        }

        impl<R: ReceivedResources> IntoWireValue<R> for $ty {
            type Read = Self;

            fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
                frame.write_field(field_id, &value::$encode(self))
            }
        }

        impl WireTyped for $ty {
            const WIRE_TYPE: WireType = $desc;
        }
    };
}

int_wire_value!(u8, encode_u8, decode_u8, WireType::Uint(1));
int_wire_value!(u16, encode_u16, decode_u16, WireType::Uint(2));
int_wire_value!(u32, encode_u32, decode_u32, WireType::Uint(4));
int_wire_value!(u64, encode_u64, decode_u64, WireType::Uint(8));
int_wire_value!(i8, encode_i8, decode_i8, WireType::Int(1));
int_wire_value!(i16, encode_i16, decode_i16, WireType::Int(2));
int_wire_value!(i32, encode_i32, decode_i32, WireType::Int(4));
int_wire_value!(i64, encode_i64, decode_i64, WireType::Int(8));
int_wire_value!(bool, encode_bool, decode_bool, WireType::Bool);

macro_rules! nonzero_wire_value {
    ($ty:ty, $raw:ty) => {
        impl<'a, R: ReceivedResources> WireValue<'a, R> for $ty {
            type Owned = Self;

            fn from_field(data: &'a [u8], frame: &mut R) -> Result<Self, IpcError> {
                <$ty>::new(<$raw as WireValue<'a, R>>::from_field(data, frame)?)
                    .ok_or(IpcError::InvalidValue)
            }
        }

        impl<R: ReceivedResources> IntoWireValue<R> for $ty {
            type Read = Self;

            fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
                self.get().write_into(frame, field_id)
            }
        }

        impl WireTyped for $ty {
            const WIRE_TYPE: WireType = <$raw as WireTyped>::WIRE_TYPE;
        }
    };
}

nonzero_wire_value!(NonZeroU16, u16);
nonzero_wire_value!(NonZeroU32, u32);

impl<'a, R: ReceivedResources> WireValue<'a, R> for &'a str {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        value::decode_str(data)
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for &str {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, self.as_bytes())
    }
}

impl WireTyped for &str {
    const WIRE_TYPE: WireType = WireType::Str;
}

impl<'a, R: ReceivedResources> WireValue<'a, R> for &'a [u8] {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        Ok(data)
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for &[u8] {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, self)
    }
}

impl WireTyped for &[u8] {
    const WIRE_TYPE: WireType = WireType::Bytes;
}

impl<'a, R: ReceivedResources> WireValue<'a, R> for () {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        if data.is_empty() {
            Ok(())
        } else {
            Err(IpcError::InvalidValue)
        }
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for () {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, &[])
    }
}

impl WireTyped for () {
    const WIRE_TYPE: WireType = WireType::BoundedBytes(0);
}

/// Поле-capability забирает слот своего индекса. Индекс пустого либо уже взятого слота -
/// `InvalidValue`: у capability кадра не бывает двух владельцев.
impl<'a, R: ReceivedResources, K: ObjectKind> WireValue<'a, R> for Cap<K> {
    type Owned = R::Handle<K>;

    fn from_field(data: &'a [u8], frame: &mut R) -> Result<R::Handle<K>, IpcError> {
        frame
            .take_handle(value::decode_port_index(data)?)
            .ok_or(IpcError::InvalidValue)
    }
}

impl<K: ObjectKind> WireTyped for Cap<K> {
    const WIRE_TYPE: WireType = WireType::Capability;
}

impl<'a, R: ReceivedResources, T: WireValue<'a, R> + WireTyped> WireValue<'a, R> for Option<T> {
    type Owned = Option<T::Owned>;

    fn from_field(data: &'a [u8], frame: &mut R) -> Result<Self::Owned, IpcError> {
        const { _ = <Self as WireTyped>::WIRE_TYPE };
        if data.is_empty() {
            Ok(None)
        } else {
            T::from_field(data, frame).map(Some)
        }
    }
}

impl<R: ReceivedResources, T: IntoWireValue<R, Read: WireTyped>> IntoWireValue<R> for Option<T> {
    type Read = Option<T::Read>;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        const { _ = <Self::Read as WireTyped>::WIRE_TYPE };
        match self {
            Some(value) => value.write_into(frame, field_id),
            None => frame.write_field(field_id, &[]),
        }
    }
}

impl<T: WireTyped> WireTyped for Option<T> {
    const WIRE_TYPE: WireType = {
        assert!(
            schema::is_never_empty(T::WIRE_TYPE),
            "Option<T> needs a value whose field is never empty"
        );
        WireType::Optional(&T::WIRE_TYPE)
    };
}

/// Исход `Result` - вложенная запись с единственным полем: успех под `field_id = 1`, доменная
/// ошибка под `field_id = 2`. Запись без обоих полей - `MissingField`, с обоими -
/// `InvalidValue`.
impl<'a, R, T, E> WireValue<'a, R> for Result<T, E>
where
    R: ReceivedResources,
    T: WireValue<'a, R>,
    E: WireValue<'a, R>,
{
    type Owned = Result<T::Owned, E::Owned>;

    fn from_field(data: &'a [u8], frame: &mut R) -> Result<Self::Owned, IpcError> {
        let field = outcome_field(data)?;
        if field.id == RESULT_OK_FIELD {
            T::from_field(field.data, frame).map(Ok)
        } else {
            E::from_field(field.data, frame).map(Err)
        }
    }
}

/// Успешный исход - значение `T`, в том числе владеющее capability. Доменная ошибка `E` читается
/// тем же типом, которым записана.
impl<R, T, E> IntoWireValue<R> for Result<T, E>
where
    R: ReceivedResources,
    T: IntoWireValue<R>,
    E: IntoWireValue<R, Read = E>,
{
    type Read = Result<T::Read, E>;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_nested(field_id, |outcome| match self {
            Ok(value) => value.write_into(outcome, RESULT_OK_FIELD),
            Err(error) => error.write_into(outcome, RESULT_ERR_FIELD),
        })
    }
}

impl<T: WireTyped, E: WireTyped> WireTyped for Result<T, E> {
    const WIRE_TYPE: WireType = WireType::Result {
        ok: &T::WIRE_TYPE,
        err: &E::WIRE_TYPE,
    };
}

pub(crate) fn outcome_field(data: &[u8]) -> Result<Field<'_>, IpcError> {
    let mut outcome = None;
    let mut cursor = FieldCursor::new(data);
    while let Some(field) = cursor.next_field()? {
        if field.id != RESULT_OK_FIELD && field.id != RESULT_ERR_FIELD {
            continue;
        }
        if outcome.replace(field).is_some() {
            return Err(IpcError::InvalidValue);
        }
    }
    outcome.ok_or(IpcError::MissingField)
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use syscall::kind;

    use super::{IntoWireValue, WireValue};
    use crate::{
        resources::testing::{TestCap, TestFrame, cap},
        wire::{Cap, FieldCursor, IpcError, MessageBuf},
    };

    const FIELD_ID: u8 = 1;

    fn port(raw: u32) -> TestCap<kind::Port> {
        cap(raw)
    }

    fn encode<W: IntoWireValue<TestFrame>>(value: W) -> (Vec<u8>, TestFrame) {
        let mut buf = MessageBuf::new();
        value.write_into(&mut buf, FIELD_ID).expect("write ok");
        buf.finish().expect("finish ok");
        (buf.as_bytes().to_vec(), buf.into_resources())
    }

    fn only_field(bytes: &[u8]) -> &[u8] {
        let mut cursor = FieldCursor::new(bytes);
        let field = cursor.next_field().expect("decode ok").expect("present");
        assert_eq!(field.id, FIELD_ID);
        field.data
    }

    fn round_trip<W, O>(value: W) -> O
    where
        W: IntoWireValue<TestFrame>,
        W::Read: for<'b> WireValue<'b, TestFrame, Owned = O>,
    {
        let (bytes, mut frame) = encode(value);
        <W::Read as WireValue<'_, TestFrame>>::from_field(only_field(&bytes), &mut frame)
            .expect("from_field ok")
    }

    fn field_with(data: &[u8]) -> Vec<u8> {
        let mut buf: MessageBuf = MessageBuf::new();
        buf.write_field(FIELD_ID, data).expect("write ok");
        buf.finish().expect("finish ok");
        buf.as_bytes().to_vec()
    }

    #[test]
    fn scalars_round_trip() {
        macro_rules! check {
            ($($ty:ty = $val:expr),*) => {$({
                let value: $ty = $val;
                assert_eq!(round_trip(value), value);
            })*};
        }
        check!(
            u8 = 0xAB,
            u16 = 0xABCD,
            u32 = 0x0123_4567,
            u64 = 0x0123_4567_89AB_CDEF,
            i8 = -5,
            i16 = -300,
            i32 = -70_000,
            i64 = -5_000_000_000,
            bool = true,
            bool = false
        );
    }

    #[test]
    fn a_nonzero_integer_round_trips_and_zero_is_refused() {
        let value = core::num::NonZeroU16::new(0xABCD).expect("nonzero");
        let zero = field_with(&0u16.to_le_bytes());

        assert_eq!(round_trip(value), value);
        assert_eq!(
            <core::num::NonZeroU16 as WireValue<'_, TestFrame>>::from_field(
                only_field(&zero),
                &mut TestFrame::default(),
            ),
            Err(IpcError::InvalidValue)
        );
    }

    crate::wire_value! {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct Cursor(u32);
    }

    #[test]
    fn a_newtype_travels_as_its_inner_value() {
        let (bytes, _) = encode(Cursor(0x0123_4567));

        assert_eq!(only_field(&bytes), &0x0123_4567u32.to_le_bytes());
        assert_eq!(round_trip(Cursor(7)), Cursor(7));
    }

    #[test]
    fn strings_and_bytes_round_trip() {
        let (encoded, mut frame) = encode("hello");
        let text = <&str as WireValue<TestFrame>>::from_field(only_field(&encoded), &mut frame);
        assert_eq!(text, Ok("hello"));
        let (encoded, mut frame) = encode(&[1u8, 2, 3][..]);
        let bytes = <&[u8] as WireValue<TestFrame>>::from_field(only_field(&encoded), &mut frame);
        assert_eq!(bytes, Ok(&[1u8, 2, 3][..]));
    }

    #[test]
    fn a_cap_travels_in_a_slot_and_its_slot_is_taken_once() {
        let (encoded, mut frame) = encode(port(0x2A));
        let data = only_field(&encoded);

        // Хэндл передаётся слотом кадра, в теле лежит только его индекс.
        assert_eq!(data, [0]);
        assert_eq!(
            <Cap<kind::Port> as WireValue<TestFrame>>::from_field(data, &mut frame),
            Ok(port(0x2A))
        );
        // Второе поле с тем же индексом - это второй владелец того же хэндла.
        assert_eq!(
            <Cap<kind::Port> as WireValue<TestFrame>>::from_field(data, &mut frame),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn a_cap_index_without_a_slot_is_refused() {
        let buf = field_with(&[3]);
        let (_, mut frame) = encode(port(0x2A));
        assert_eq!(
            <Cap<kind::Port> as WireValue<TestFrame>>::from_field(only_field(&buf), &mut frame),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn an_empty_value_is_a_field_without_data() {
        round_trip(());

        let with_data = field_with(&[0]);
        assert_eq!(
            <() as WireValue<()>>::from_field(only_field(&with_data), &mut ()),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn an_optional_cap_round_trips_in_both_states() {
        assert_eq!(round_trip(None::<TestCap<kind::Port>>), None);
        assert_eq!(round_trip(Some(port(0x2A))), Some(port(0x2A)));
    }

    #[test]
    fn an_optional_integer_round_trips_in_both_states() {
        for expected in [None, Some(0u32), Some(7)] {
            assert_eq!(round_trip(expected), expected);
        }
    }

    crate::wire_value! {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct Span {
            start: u32,
            len: u16,
        }
    }

    #[test]
    fn an_optional_aggregate_round_trips_in_both_states() {
        for expected in [
            None,
            Some(Span { start: 0, len: 0 }),
            Some(Span { start: 7, len: 3 }),
        ] {
            assert_eq!(round_trip(expected), expected);
        }
    }

    #[test]
    fn frame_rejects_more_caps_than_it_carries() {
        let mut buf = MessageBuf::new();
        for id in 0..crate::wire::MESSAGE_MAX_HANDLES {
            port(id as u32 + 1)
                .write_into(&mut buf, FIELD_ID + id as u8)
                .expect("within the frame budget");
        }
        assert_eq!(
            port(99).write_into(&mut buf, 99),
            Err(IpcError::TooManyHandles)
        );
    }

    #[test]
    fn both_outcomes_of_a_result_round_trip() {
        for expected in [Ok(7u32), Err(-3i16)] {
            assert_eq!(round_trip(expected), expected);
        }
    }

    #[test]
    fn a_result_carries_the_capability_of_its_outcome() {
        let sent: Result<TestCap<kind::Port>, u8> = Ok(port(0x2A));
        assert_eq!(round_trip(sent), Ok(port(0x2A)));
    }

    #[test]
    fn a_result_without_an_outcome_is_refused() {
        let empty = field_with(&[0]);
        assert_eq!(
            <Result<u32, u8> as WireValue<()>>::from_field(only_field(&empty), &mut ()),
            Err(IpcError::MissingField)
        );
    }

    #[test]
    fn a_result_with_both_outcomes_is_refused() {
        let mut inner: MessageBuf = MessageBuf::new();
        inner
            .write_field(1, &crate::wire::value::encode_u32(7))
            .expect("ok");
        inner.write_field(2, &[3]).expect("err");
        inner.finish().expect("finish ok");

        assert_eq!(
            <Result<u32, u8> as WireValue<()>>::from_field(inner.as_bytes(), &mut ()),
            Err(IpcError::InvalidValue)
        );
    }
}
