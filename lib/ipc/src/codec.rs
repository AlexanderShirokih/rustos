//! Кодек записи поля: чтение [`WireValue`], запись [`IntoWireValue`] и дескриптор схемы
//! [`WireTyped`].

use syscall::ObjectKind;

use crate::{
    ReceivedResources,
    schema::{self, WireType},
    wire::{Bytes, Cap, FieldCursor, IpcError, MessageBuf, Str, value},
};

const RESULT_OK_FIELD: u8 = 1;
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
    fn from_field(data: &'a [u8], frame: &mut R) -> Result<Self::Owned, IpcError>;
}

/// Значение исходящего кадра, которое пишет одну запись поля.
///
/// Значение, владеющее capability, держит их до итога передачи. Успех отдаёт capability
/// получателю, отказ оставляет их отправителю, и освобождение значения их закрывает. После
/// `Timeout` вызова значение освобождается и тогда, когда его capability уже у сервера.
pub trait IntoWireValue: Sized {
    /// Тип, которым получатель читает эту запись.
    type Read;

    /// Пишет запись поля `field_id`. Capability значения занимают слоты кадра `buf`.
    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError>;

    /// Отдаёт владение после успешной передачи: capability забрало ядро, и закрытие
    /// подавлено. Значение без capability ничего не отдаёт.
    fn relinquish(self);
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

        impl IntoWireValue for $ty {
            type Read = Self;

            fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
                buf.write_field(field_id, &value::$encode(*self))
            }

            fn relinquish(self) {}
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

impl<'a, R: ReceivedResources, const N: usize> WireValue<'a, R> for Str<'a, N> {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        Str::new(value::decode_str(data)?).ok_or(IpcError::InvalidValue)
    }
}

impl<const N: usize> IntoWireValue for Str<'_, N> {
    type Read = Self;

    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
        buf.write_field(field_id, self.as_str().as_bytes())
    }

    fn relinquish(self) {}
}

impl<const N: usize> WireTyped for Str<'_, N> {
    const WIRE_TYPE: WireType = WireType::BoundedStr(N);
}

impl<'a, R: ReceivedResources, const N: usize> WireValue<'a, R> for Bytes<'a, N> {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        Bytes::new(data).ok_or(IpcError::InvalidValue)
    }
}

impl<const N: usize> IntoWireValue for Bytes<'_, N> {
    type Read = Self;

    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
        buf.write_field(field_id, self.as_bytes())
    }

    fn relinquish(self) {}
}

impl<const N: usize> WireTyped for Bytes<'_, N> {
    const WIRE_TYPE: WireType = WireType::BoundedBytes(N);
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

impl IntoWireValue for () {
    type Read = Self;

    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
        buf.write_field(field_id, &[])
    }

    fn relinquish(self) {}
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

impl<K: ObjectKind> IntoWireValue for Cap<K> {
    type Read = Self;

    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
        buf.write_handle(field_id, self.raw().get())
    }

    fn relinquish(self) {
        // Запись не владеет хэндлом, отдавать нечего.
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

impl<T: IntoWireValue<Read: WireTyped>> IntoWireValue for Option<T> {
    type Read = Option<T::Read>;

    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
        const { _ = <Self::Read as WireTyped>::WIRE_TYPE };
        match self {
            Some(value) => value.write_as_field(buf, field_id),
            None => buf.write_field(field_id, &[]),
        }
    }

    fn relinquish(self) {
        if let Some(value) = self {
            value.relinquish();
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
        let mut outcome = None;
        let mut cursor = FieldCursor::new(data);
        while let Some(field) = cursor.next_field()? {
            let decoded = match field.id {
                RESULT_OK_FIELD => Ok(T::from_field(field.data, frame)?),
                RESULT_ERR_FIELD => Err(E::from_field(field.data, frame)?),
                _ => continue,
            };
            if outcome.replace(decoded).is_some() {
                return Err(IpcError::InvalidValue);
            }
        }
        outcome.ok_or(IpcError::MissingField)
    }
}

/// Успешный исход - значение `T`, в том числе владеющее capability. Доменная ошибка `E` читается
/// тем же типом, которым записана.
impl<T: IntoWireValue, E: IntoWireValue<Read = E>> IntoWireValue for Result<T, E> {
    type Read = Result<T::Read, E>;

    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
        buf.write_nested(field_id, |outcome| match self {
            Ok(value) => value.write_as_field(outcome, RESULT_OK_FIELD),
            Err(error) => error.write_as_field(outcome, RESULT_ERR_FIELD),
        })
    }

    fn relinquish(self) {
        match self {
            Ok(value) => value.relinquish(),
            Err(error) => error.relinquish(),
        }
    }
}

impl<T: WireTyped, E: WireTyped> WireTyped for Result<T, E> {
    const WIRE_TYPE: WireType = WireType::Result {
        ok: &T::WIRE_TYPE,
        err: &E::WIRE_TYPE,
    };
}

#[cfg(test)]
mod tests {
    use syscall::{Handle, kind};

    use super::{IntoWireValue, WireValue};
    use crate::{
        resources::testing::TestFrame,
        wire::{Bytes, Cap, FieldCursor, IpcError, MessageBuf, Str},
    };

    const FIELD_ID: u8 = 1;

    fn port(raw: u32) -> Cap<kind::Port> {
        Cap::from_handle(Handle::from_abi(
            core::num::NonZeroU32::new(raw).expect("non-zero"),
        ))
    }

    fn encode<'b, V: IntoWireValue>(v: &V, buf: &'b mut MessageBuf) -> (&'b [u8], TestFrame) {
        v.write_as_field(buf, FIELD_ID).expect("write ok");
        buf.finish().expect("finish ok");
        let buf: &'b MessageBuf = buf;
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        assert_eq!(field.id, FIELD_ID);
        (field.data, TestFrame::of(buf.handles()))
    }

    fn round_trip<'b, V>(v: &V, buf: &'b mut MessageBuf) -> V
    where
        V: IntoWireValue + WireValue<'b, TestFrame, Owned = V>,
    {
        let (data, mut frame) = encode(v, buf);
        V::from_field(data, &mut frame).expect("from_field ok")
    }

    fn field_with(data: &[u8]) -> MessageBuf {
        let mut buf = MessageBuf::new();
        buf.write_field(FIELD_ID, data).expect("write ok");
        buf.finish().expect("finish ok");
        buf
    }

    fn only_field(buf: &MessageBuf) -> &[u8] {
        let mut cursor = FieldCursor::new(buf.as_bytes());
        cursor.next_field().expect("ok").expect("present").data
    }

    #[test]
    fn scalars_round_trip() {
        macro_rules! check {
            ($($ty:ty = $val:expr),*) => {$({
                let value: $ty = $val;
                assert_eq!(round_trip(&value, &mut MessageBuf::new()), value);
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
    fn bounded_values_round_trip_and_refuse_data_over_their_bound() {
        let mut buf = MessageBuf::new();
        let text: Str<8> = round_trip(&Str::new("hello").expect("within bound"), &mut buf);
        assert_eq!(text.as_str(), "hello");
        let mut buf = MessageBuf::new();
        let bytes: Bytes<8> = round_trip(&Bytes::new(&[1, 2, 3]).expect("within bound"), &mut buf);
        assert_eq!(bytes.as_bytes(), &[1, 2, 3]);

        // Отправитель записал больше, чем объявленная граница получателя.
        let long = field_with(b"abcd");
        assert_eq!(
            <Str<3> as WireValue<()>>::from_field(only_field(&long), &mut ()),
            Err(IpcError::InvalidValue)
        );
        assert_eq!(
            <Bytes<3> as WireValue<()>>::from_field(only_field(&long), &mut ()),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn a_cap_travels_in_a_slot_and_its_slot_is_taken_once() {
        let cap = port(0x2A);
        let mut buf = MessageBuf::new();
        let (data, mut frame) = encode(&cap, &mut buf);

        // Хэндл передаётся слотом кадра, в теле лежит только его индекс.
        assert_eq!(data, [0]);
        assert_eq!(
            <Cap<kind::Port> as WireValue<TestFrame>>::from_field(data, &mut frame),
            Ok(cap)
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
        assert_eq!(
            <Cap<kind::Port> as WireValue<TestFrame>>::from_field(
                only_field(&buf),
                &mut TestFrame::of(&[0x2A])
            ),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn an_empty_value_is_a_field_without_data() {
        let mut buf = MessageBuf::new();
        round_trip(&(), &mut buf);

        let with_data = field_with(&[0]);
        assert_eq!(
            <() as WireValue<()>>::from_field(only_field(&with_data), &mut ()),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn an_optional_cap_round_trips_in_both_states() {
        for expected in [None, Some(port(0x2A))] {
            let mut buf = MessageBuf::new();
            let (data, mut frame) = encode(&expected, &mut buf);
            assert_eq!(
                <Option<Cap<kind::Port>> as WireValue<TestFrame>>::from_field(data, &mut frame),
                Ok(expected)
            );
        }
    }

    #[test]
    fn an_optional_integer_round_trips_in_both_states() {
        for expected in [None, Some(0u32), Some(7)] {
            let mut buf = MessageBuf::new();
            assert_eq!(round_trip(&expected, &mut buf), expected);
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
            let mut buf = MessageBuf::new();
            assert_eq!(round_trip(&expected, &mut buf), expected);
        }
    }

    #[test]
    fn frame_rejects_more_caps_than_it_carries() {
        let mut buf = MessageBuf::new();
        for id in 0..crate::wire::MESSAGE_MAX_HANDLES {
            port(id as u32 + 1)
                .write_as_field(&mut buf, FIELD_ID + id as u8)
                .expect("within the frame budget");
        }
        assert_eq!(
            port(99).write_as_field(&mut buf, 99),
            Err(IpcError::TooManyHandles)
        );
    }

    #[test]
    fn both_outcomes_of_a_result_round_trip() {
        for expected in [Ok(7u32), Err(-3i16)] {
            let mut buf = MessageBuf::new();
            assert_eq!(round_trip(&expected, &mut buf), expected);
        }
    }

    #[test]
    fn a_result_carries_the_capability_of_its_outcome() {
        let expected: Result<Cap<kind::Port>, u8> = Ok(port(0x2A));
        let mut buf = MessageBuf::new();
        assert_eq!(round_trip(&expected, &mut buf), expected);
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
        let mut inner = MessageBuf::new();
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
