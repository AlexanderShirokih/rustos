//! Кодирование полей протокола.

use crate::{
    schema::WireType,
    wire::{
        Bytes, Cap, FIELD_ID_MIN, FLAG_DOMAIN_ERR, FieldCursor, HandleSlots, Header, IpcError,
        MessageBuf, Str, value,
    },
};

/// Значение, занимающее одну запись поля.
/// Заимствование результата декодирования из `data` имеет lifetime `'a`.
/// Для owned-типов (целые, `bool`) `'a` не задействован.
pub trait WireValue<'a>: Sized {
    /// Кодирует значение в `data` и дописывает запись `field_id` в `buf`.
    /// Значение-капа кладёт handle-id в `handles`, а в тело пишет его индекс.
    fn write_as_field<const N: usize>(
        &self,
        buf: &mut MessageBuf<N>,
        field_id: u8,
        handles: &mut HandleSlots,
    ) -> Result<(), IpcError>;

    /// Декодирует значение из `data` записи поля. `handles` - слоты кадра, из
    /// которых значение-капа достаёт свой handle по индексу.
    fn from_field(data: &'a [u8], handles: &HandleSlots) -> Result<Self, IpcError>;
}

/// Значение ответа two-way операции занимает единственную запись поля кадра-ответа.
///
/// Значение передаётся как есть, а доменная ошибка `Result<T, E>` - под флагом
/// [`FLAG_DOMAIN_ERR`] с телом из `E`.
pub trait WireReply<'a>: Sized {
    /// Флаги заголовка, которые содержит этот ответ поверх `FLAG_RESPONSE`.
    fn reply_flags(&self) -> u16;

    /// Кодирует значение в тело ответа.
    fn write_reply<const N: usize>(
        &self,
        buf: &mut MessageBuf<N>,
        handles: &mut HandleSlots,
    ) -> Result<(), IpcError>;

    /// Восстанавливает значение из тела ответа, разбирая флаги заголовка.
    /// Пустое тело ответа - `MissingField`.
    fn read_reply(header: &Header, body: &'a [u8], handles: &HandleSlots)
    -> Result<Self, IpcError>;
}

fn read_only_field<'a, V: WireValue<'a>>(
    body: &'a [u8],
    handles: &HandleSlots,
) -> Result<V, IpcError> {
    let mut cursor = FieldCursor::new(body);
    let mut value = None;
    while let Some(field) = cursor.next_field()? {
        if field.id == FIELD_ID_MIN {
            value = Some(V::from_field(field.data, handles)?);
        }
    }
    value.ok_or(IpcError::MissingField)
}

impl<'a, T: WireValue<'a>> WireReply<'a> for T {
    fn reply_flags(&self) -> u16 {
        0
    }

    fn write_reply<const N: usize>(
        &self,
        buf: &mut MessageBuf<N>,
        handles: &mut HandleSlots,
    ) -> Result<(), IpcError> {
        self.write_as_field(buf, FIELD_ID_MIN, handles)
    }

    fn read_reply(
        _header: &Header,
        body: &'a [u8],
        handles: &HandleSlots,
    ) -> Result<Self, IpcError> {
        read_only_field(body, handles)
    }
}

impl<'a, T: WireValue<'a>, E: WireValue<'a>> WireReply<'a> for Result<T, E> {
    fn reply_flags(&self) -> u16 {
        if self.is_err() { FLAG_DOMAIN_ERR } else { 0 }
    }

    fn write_reply<const N: usize>(
        &self,
        buf: &mut MessageBuf<N>,
        handles: &mut HandleSlots,
    ) -> Result<(), IpcError> {
        match self {
            Ok(ok) => ok.write_as_field(buf, FIELD_ID_MIN, handles),
            Err(err) => err.write_as_field(buf, FIELD_ID_MIN, handles),
        }
    }

    fn read_reply(
        header: &Header,
        body: &'a [u8],
        handles: &HandleSlots,
    ) -> Result<Self, IpcError> {
        if header.has_flag(FLAG_DOMAIN_ERR) {
            Ok(Err(read_only_field(body, handles)?))
        } else {
            Ok(Ok(read_only_field(body, handles)?))
        }
    }
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
                _handles: &mut HandleSlots,
            ) -> Result<(), IpcError> {
                buf.write_field(field_id, &value::$encode(*self))
            }

            fn from_field(data: &'a [u8], _handles: &HandleSlots) -> Result<Self, IpcError> {
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
        _handles: &mut HandleSlots,
    ) -> Result<(), IpcError> {
        buf.write_field(field_id, &value::encode_bool(*self))
    }

    fn from_field(data: &'a [u8], _handles: &HandleSlots) -> Result<Self, IpcError> {
        value::decode_bool(data)
    }
}

impl<'a, const N: usize> WireValue<'a> for Str<'a, N> {
    fn write_as_field<const M: usize>(
        &self,
        buf: &mut MessageBuf<M>,
        field_id: u8,
        _handles: &mut HandleSlots,
    ) -> Result<(), IpcError> {
        buf.write_field(field_id, self.as_str().as_bytes())
    }

    fn from_field(data: &'a [u8], _handles: &HandleSlots) -> Result<Self, IpcError> {
        Str::new(value::decode_str(data)?).ok_or(IpcError::InvalidValue)
    }
}

impl<'a, const N: usize> WireValue<'a> for Bytes<'a, N> {
    fn write_as_field<const M: usize>(
        &self,
        buf: &mut MessageBuf<M>,
        field_id: u8,
        _handles: &mut HandleSlots,
    ) -> Result<(), IpcError> {
        buf.write_field(field_id, self.as_bytes())
    }

    fn from_field(data: &'a [u8], _handles: &HandleSlots) -> Result<Self, IpcError> {
        Bytes::new(data).ok_or(IpcError::InvalidValue)
    }
}

impl WireTyped for Cap {
    const WIRE_TYPE: WireType = WireType::Capability;
}

impl<'a> WireValue<'a> for Cap {
    fn write_as_field<const N: usize>(
        &self,
        buf: &mut MessageBuf<N>,
        field_id: u8,
        handles: &mut HandleSlots,
    ) -> Result<(), IpcError> {
        let index = handles.push(self.raw().get())?;
        buf.write_field(field_id, &value::encode_port_index(index))
    }

    fn from_field(data: &'a [u8], handles: &HandleSlots) -> Result<Self, IpcError> {
        let raw = handles.take(value::decode_port_index(data)?)?;
        let id = core::num::NonZeroU32::new(raw).ok_or(IpcError::InvalidValue)?;
        Ok(Cap::from_raw(id))
    }
}

#[cfg(test)]
mod tests {
    use super::WireValue;
    use crate::wire::{
        Bytes, Cap, FieldCursor, HandleSlots, IpcError, MESSAGE_INLINE_MAX, MessageBuf, Str,
    };

    const FIELD_ID: u8 = 1;

    fn round_trip<'b, V>(v: &V, buf: &'b mut MessageBuf<MESSAGE_INLINE_MAX>) -> V
    where
        V: WireValue<'b> + core::fmt::Debug,
    {
        let mut handles = HandleSlots::new();
        v.write_as_field(buf, FIELD_ID, &mut handles)
            .expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        assert_eq!(field.id, FIELD_ID);
        V::from_field(field.data, &handles).expect("from_field ok")
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
            <Str<3> as WireValue>::from_field(field.data, &HandleSlots::new()),
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
            <Bytes<2> as WireValue>::from_field(field.data, &HandleSlots::new()),
            Err(IpcError::InvalidValue)
        );
    }
    #[test]
    fn cap_round_trip_through_the_handle_slots() {
        let cap = Cap::from_raw(core::num::NonZeroU32::new(0x2A).expect("non-zero"));
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        let mut handles = HandleSlots::new();
        cap.write_as_field(&mut buf, FIELD_ID, &mut handles)
            .expect("write ok");
        buf.finish().expect("finish ok");

        // Handle уехал слотом кадра, в теле лежит только его индекс.
        assert_eq!(handles.as_slice(), [0x2A]);

        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("ok").expect("present");
        let received = HandleSlots::from_received(handles.as_slice());
        assert_eq!(
            <Cap as WireValue>::from_field(field.data, &received),
            Ok(cap)
        );
    }

    #[test]
    fn cap_from_field_consumes_its_slot_once() {
        let cap = Cap::from_raw(core::num::NonZeroU32::new(0x2A).expect("non-zero"));
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        let mut handles = HandleSlots::new();
        cap.write_as_field(&mut buf, FIELD_ID, &mut handles)
            .expect("write ok");
        buf.finish().expect("finish ok");

        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("ok").expect("present");
        let received = HandleSlots::from_received(handles.as_slice());
        assert_eq!(
            <Cap as WireValue>::from_field(field.data, &received),
            Ok(cap)
        );
        // Второе поле с тем же индексом - это второй владелец того же хэндла.
        assert_eq!(
            <Cap as WireValue>::from_field(field.data, &received),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn cap_from_field_rejects_an_index_without_a_slot() {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_field(FIELD_ID, &[3]).expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("ok").expect("present");
        assert_eq!(
            <Cap as WireValue>::from_field(field.data, &HandleSlots::new()),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn handle_slots_reject_more_caps_than_the_frame_carries() {
        let mut handles = HandleSlots::new();
        for id in 0..crate::wire::MESSAGE_MAX_HANDLES {
            handles
                .push(id as u32 + 1)
                .expect("within the frame budget");
        }
        assert_eq!(handles.push(99), Err(IpcError::TooManyHandles));
    }
}
