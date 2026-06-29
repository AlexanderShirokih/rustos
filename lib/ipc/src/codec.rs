//! Кодирование полей протокола.

use crate::{
    schema::WireType,
    wire::{Bytes, IpcError, MessageBuf, Str, value},
};

/// Значение, занимающее одну запись поля.
/// Заимствование результата декодирования из `data` несёт lifetime `'a`.
/// Для owned-типов (целые, `bool`) `'a` не задействован.
pub trait WireValue<'a>: Sized {
    /// Кодирует значение в `data` и дописывает запись `field_id` в `buf`.
    fn write_as_field<const N: usize>(
        &self,
        buf: &mut MessageBuf<N>,
        field_id: u8,
    ) -> Result<(), IpcError>;

    /// Декодирует значение из `data` записи поля.
    fn from_field(data: &'a [u8]) -> Result<Self, IpcError>;
}

/// Структурный wire-дескриптор типа для схемы протокола.
/// Лайфтайм-свободный: `WIRE_TYPE` нужен в const-контексте дескриптора `DESC`,
/// где имя лайфтайма недоступно. Агрегат собирает дескриптор из полей.
pub trait WireTyped {
    /// Дескриптор раскладки значения в сообщении.
    const WIRE_TYPE: WireType;
}

macro_rules! int_wire_value {
    ($ty:ty, $encode:ident, $decode:ident, $desc:expr) => {
        impl<'a> WireValue<'a> for $ty {
            fn write_as_field<const N: usize>(
                &self,
                buf: &mut MessageBuf<N>,
                field_id: u8,
            ) -> Result<(), IpcError> {
                buf.write_field(field_id, &value::$encode(*self))
            }

            fn from_field(data: &'a [u8]) -> Result<Self, IpcError> {
                value::$decode(data)
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

impl WireTyped for bool {
    const WIRE_TYPE: WireType = WireType::Bool;
}

impl<const N: usize> WireTyped for Str<'_, N> {
    const WIRE_TYPE: WireType = WireType::BoundedStr(N);
}

impl<const N: usize> WireTyped for Bytes<'_, N> {
    const WIRE_TYPE: WireType = WireType::BoundedBytes(N);
}

impl<'a> WireValue<'a> for bool {
    fn write_as_field<const N: usize>(
        &self,
        buf: &mut MessageBuf<N>,
        field_id: u8,
    ) -> Result<(), IpcError> {
        buf.write_field(field_id, &value::encode_bool(*self))
    }

    fn from_field(data: &'a [u8]) -> Result<Self, IpcError> {
        value::decode_bool(data)
    }
}

impl<'a, const N: usize> WireValue<'a> for Str<'a, N> {
    fn write_as_field<const M: usize>(
        &self,
        buf: &mut MessageBuf<M>,
        field_id: u8,
    ) -> Result<(), IpcError> {
        buf.write_field(field_id, self.as_str().as_bytes())
    }

    fn from_field(data: &'a [u8]) -> Result<Self, IpcError> {
        Str::new(value::decode_str(data)?).ok_or(IpcError::InvalidValue)
    }
}

impl<'a, const N: usize> WireValue<'a> for Bytes<'a, N> {
    fn write_as_field<const M: usize>(
        &self,
        buf: &mut MessageBuf<M>,
        field_id: u8,
    ) -> Result<(), IpcError> {
        buf.write_field(field_id, self.as_bytes())
    }

    fn from_field(data: &'a [u8]) -> Result<Self, IpcError> {
        Bytes::new(data).ok_or(IpcError::InvalidValue)
    }
}

#[cfg(test)]
mod tests {
    use super::WireValue;
    use crate::wire::{Bytes, FieldCursor, IpcError, MESSAGE_INLINE_MAX, MessageBuf, Str};

    const FIELD_ID: u8 = 1;

    fn round_trip<'b, V>(v: &V, buf: &'b mut MessageBuf<MESSAGE_INLINE_MAX>) -> V
    where
        V: WireValue<'b> + core::fmt::Debug,
    {
        v.write_as_field(buf, FIELD_ID).expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        assert_eq!(field.id, FIELD_ID);
        V::from_field(field.data).expect("from_field ok")
    }

    macro_rules! int_round_trip_test {
        ($name:ident, $ty:ty, $val:expr) => {
            #[test]
            fn $name() {
                let v: $ty = $val;
                let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
                assert_eq!(round_trip(&v, &mut buf), v);
            }
        };
    }

    int_round_trip_test!(u8_round_trip, u8, 0xAB);
    int_round_trip_test!(u16_round_trip, u16, 0xABCD);
    int_round_trip_test!(u32_round_trip, u32, 0x0123_4567);
    int_round_trip_test!(u64_round_trip, u64, 0x0123_4567_89AB_CDEF);
    int_round_trip_test!(i8_round_trip, i8, -5);
    int_round_trip_test!(i16_round_trip, i16, -300);
    int_round_trip_test!(i32_round_trip, i32, -70_000);
    int_round_trip_test!(i64_round_trip, i64, -5_000_000_000);

    #[test]
    fn bool_round_trip() {
        let mut buf_true = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        assert!(round_trip(&true, &mut buf_true));
        let mut buf_false = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        assert!(!round_trip(&false, &mut buf_false));
    }

    #[test]
    fn str_round_trip() {
        let s = Str::<8>::new("hello").expect("within bound");
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        let decoded: Str<8> = round_trip(&s, &mut buf);
        assert_eq!(decoded.as_str(), "hello");
    }

    #[test]
    fn bytes_round_trip() {
        let b = Bytes::<8>::new(&[1, 2, 3, 4]).expect("within bound");
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        let decoded: Bytes<8> = round_trip(&b, &mut buf);
        assert_eq!(decoded.as_bytes(), &[1, 2, 3, 4]);
    }

    #[test]
    fn str_from_field_rejects_over_bound() {
        // data длиннее N: декодирование в Str<N> отвергается границей.
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_field(FIELD_ID, b"abcd").expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("ok").expect("present");
        assert_eq!(
            <Str<3> as WireValue>::from_field(field.data),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn bytes_from_field_rejects_over_bound() {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_field(FIELD_ID, &[1, 2, 3, 4]).expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("ok").expect("present");
        assert_eq!(
            <Bytes<2> as WireValue>::from_field(field.data),
            Err(IpcError::InvalidValue)
        );
    }
}
