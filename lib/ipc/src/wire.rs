//! Wire-формат сообщения: заголовок, тело (table-of-fields), кодек значений,
//! стековый буфер и типизированные порты.
//!
//! Все целые - little-endian. Кадр (заголовок + тело) укладывается в
//! [`MESSAGE_INLINE_MAX`]. Кодирование без heap.

use core::{marker::PhantomData, num::NonZeroU32};

/// Максимальная длина кадра (заголовок + тело) в байтах.
pub const MESSAGE_INLINE_MAX: usize = 256;

/// Максимум хэндлов в транспортном векторе кадра.
pub const MESSAGE_MAX_HANDLES: usize = 4;

/// Размер заголовка кадра в байтах.
pub const HEADER_SIZE: usize = 14;

/// Бюджет тела (включая терминатор): `MESSAGE_INLINE_MAX - HEADER_SIZE`.
pub const BODY_MAX: usize = MESSAGE_INLINE_MAX - HEADER_SIZE;

/// Накладные расходы одной записи поля: `field_id` + `len`.
pub const FIELD_OVERHEAD: usize = 2;

/// Максимум `data` одиночного поля, занимающего кадр целиком:
/// `BODY_MAX - FIELD_OVERHEAD - 1` (терминатор).
pub const FIELD_DATA_MAX: usize = BODY_MAX - FIELD_OVERHEAD - 1;

/// Байт-терминатор тела; `field_id == 0` зарезервирован под него.
pub const TERMINATOR: u8 = 0x00;

/// Минимальный допустимый `field_id` записи.
pub const FIELD_ID_MIN: u8 = 1;

/// Максимальный допустимый `field_id` записи (`255` зарезервирован).
pub const FIELD_ID_MAX: u8 = 254;

/// Флаг: кадр - ответ на two-way вызов.
pub const FLAG_RESPONSE: u16 = 1 << 0;

/// Флаг: кадр - терминальная ошибка протокола (последний на канале).
pub const FLAG_EPITAPH: u16 = 1 << 1;

/// Флаг: операция допускает игнор неизвестного `ordinal`.
pub const FLAG_FLEXIBLE: u16 = 1 << 2;

/// Флаг: ответ несёт доменную ошибку `E`, а не успех `T`.
pub const FLAG_DOMAIN_ERR: u16 = 1 << 3;

/// Размер дискриминанта закрытого enum в сообщении (`u16` LE).
pub const ENUM_DISCRIMINANT_SIZE: usize = 2;

/// Ошибка кодека wire-формата и транспорта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    /// Буфер усечён: для чтения/записи не хватает байт.
    Truncated,
    /// Запись/кадр превысили бюджет [`MESSAGE_INLINE_MAX`].
    FrameOverflow,
    /// `field_id` вне диапазона `1..=254` либо нарушает возрастание.
    FieldOrder,
    /// Обязательное поле отсутствует в теле.
    MissingField,
    /// Длина записи не равна ожидаемому размеру фиксированного типа.
    BadLength,
    /// Значение `bool` не равно `0` или `1`.
    BadBool,
    /// Длина bounded `Str`/`Bytes` превысила границу `N`.
    BoundExceeded,
    /// Байты `Str` не образуют корректный UTF-8.
    NotUtf8,
    /// Дальний конец канала закрыт.
    PeerClosed,
    /// Операция не может завершиться без блокировки.
    WouldBlock,
    /// Истёк отведённый на ожидание срок.
    Timeout,
}

/// Заголовок кадра: `ordinal` @0, `txid` @8, `flags` @12 (14 байт, LE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Идентификатор операции (хэш канонического имени).
    pub ordinal: u64,
    /// Идентификатор транзакции; `0` - односторонняя операция.
    pub txid: u32,
    /// Битовая маска флагов кадра.
    pub flags: u16,
}

impl Header {
    /// Создаёт заголовок из полей.
    #[must_use]
    pub const fn new(ordinal: u64, txid: u32, flags: u16) -> Self {
        Self {
            ordinal,
            txid,
            flags,
        }
    }

    /// Записывает заголовок в первые [`HEADER_SIZE`] байт `buf`.
    /// `Truncated`, если буфер короче заголовка.
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), IpcError> {
        if buf.len() < HEADER_SIZE {
            return Err(IpcError::Truncated);
        }
        buf[0..8].copy_from_slice(&self.ordinal.to_le_bytes());
        buf[8..12].copy_from_slice(&self.txid.to_le_bytes());
        buf[12..14].copy_from_slice(&self.flags.to_le_bytes());
        Ok(())
    }

    /// Читает заголовок из первых [`HEADER_SIZE`] байт `bytes`.
    /// `Truncated`, если байт меньше заголовка.
    pub fn decode(bytes: &[u8]) -> Result<Self, IpcError> {
        if bytes.len() < HEADER_SIZE {
            return Err(IpcError::Truncated);
        }
        Ok(Self {
            ordinal: u64::from_le_bytes(bytes[0..8].try_into().expect("ordinal is 8 bytes")),
            txid: u32::from_le_bytes(bytes[8..12].try_into().expect("txid is 4 bytes")),
            flags: u16::from_le_bytes(bytes[12..14].try_into().expect("flags are 2 bytes")),
        })
    }

    /// Проверяет, установлен ли заданный флаг.
    #[must_use]
    pub const fn has_flag(&self, flag: u16) -> bool {
        self.flags & flag != 0
    }
}

/// Стековый буфер кадра: `[u8; N]` плюс позиция записи.
/// Кадр строится как заголовок, затем записи полей, затем терминатор.
#[derive(Debug, Clone)]
pub struct MessageBuf<const N: usize> {
    bytes: [u8; N],
    pos: usize,
    last_field_id: u8,
    terminated: bool,
}

impl<const N: usize> Default for MessageBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> MessageBuf<N> {
    /// Создаёт пустой буфер с нулевой позицией записи.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: [0u8; N],
            pos: 0,
            last_field_id: 0,
            terminated: false,
        }
    }

    /// Текущая длина записанных байт.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.pos
    }

    /// Пуст ли буфер (ничего не записано).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.pos == 0
    }

    /// Записанный префикс буфера.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.pos]
    }

    /// Записывает заголовок в начало буфера; продвигает позицию на
    /// [`HEADER_SIZE`]. Допустим только как первая запись.
    pub fn write_header(&mut self, header: &Header) -> Result<(), IpcError> {
        if self.pos != 0 {
            return Err(IpcError::FieldOrder);
        }
        if N < HEADER_SIZE {
            return Err(IpcError::FrameOverflow);
        }
        header.encode(&mut self.bytes[..HEADER_SIZE])?;
        self.pos = HEADER_SIZE;
        Ok(())
    }

    /// Записывает запись поля `field_id` с телом `data`.
    /// Держит строго возрастающий `field_id`; нарушение - `FieldOrder`.
    /// Превышение бюджета кадра - `FrameOverflow`.
    pub fn write_field(&mut self, field_id: u8, data: &[u8]) -> Result<(), IpcError> {
        if self.terminated {
            return Err(IpcError::FieldOrder);
        }
        if !(FIELD_ID_MIN..=FIELD_ID_MAX).contains(&field_id) {
            return Err(IpcError::FieldOrder);
        }
        if field_id <= self.last_field_id {
            return Err(IpcError::FieldOrder);
        }
        if data.len() > u8::MAX as usize {
            return Err(IpcError::BadLength);
        }
        // Запись + место под терминатор обязаны уместиться в буфер.
        let needed = FIELD_OVERHEAD + data.len();
        if self.pos + needed + 1 > N {
            return Err(IpcError::FrameOverflow);
        }
        self.bytes[self.pos] = field_id;
        self.bytes[self.pos + 1] = data.len() as u8;
        self.bytes[self.pos + 2..self.pos + 2 + data.len()].copy_from_slice(data);
        self.pos += needed;
        self.last_field_id = field_id;
        Ok(())
    }

    /// Дописывает терминатор `0x00` и завершает тело. Идемпотентен.
    pub fn finish(&mut self) -> Result<(), IpcError> {
        if self.terminated {
            return Ok(());
        }
        if self.pos + 1 > N {
            return Err(IpcError::FrameOverflow);
        }
        self.bytes[self.pos] = TERMINATOR;
        self.pos += 1;
        self.terminated = true;
        Ok(())
    }
}

/// Курсор последовательного чтения тела: итерирует записи полей.
/// Неизвестный `field_id` вызывающий пропускает через [`FieldCursor::next_field`].
#[derive(Debug, Clone)]
pub struct FieldCursor<'a> {
    body: &'a [u8],
    pos: usize,
    last_field_id: u8,
    done: bool,
}

/// Одна запись поля, возвращённая курсором.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field<'a> {
    /// Идентификатор поля (`1..=254`).
    pub id: u8,
    /// Тело записи длиной `len`.
    pub data: &'a [u8],
}

impl<'a> FieldCursor<'a> {
    /// Создаёт курсор над телом кадра (байты после заголовка).
    #[must_use]
    pub const fn new(body: &'a [u8]) -> Self {
        Self {
            body,
            pos: 0,
            last_field_id: 0,
            done: false,
        }
    }

    /// Возвращает следующую запись либо `None` на терминаторе/конце тела.
    /// Проверяет строгое возрастание `field_id` и достаточность `len`.
    pub fn next_field(&mut self) -> Result<Option<Field<'a>>, IpcError> {
        if self.done {
            return Ok(None);
        }
        let Some(&field_id) = self.body.get(self.pos) else {
            // Тело без терминатора трактуется как конец.
            self.done = true;
            return Ok(None);
        };
        if field_id == TERMINATOR {
            self.done = true;
            return Ok(None);
        }
        if field_id == u8::MAX {
            return Err(IpcError::FieldOrder);
        }
        if field_id <= self.last_field_id {
            return Err(IpcError::FieldOrder);
        }
        let Some(&len) = self.body.get(self.pos + 1) else {
            return Err(IpcError::Truncated);
        };
        let data_start = self.pos + FIELD_OVERHEAD;
        let data_end = data_start + len as usize;
        let Some(data) = self.body.get(data_start..data_end) else {
            return Err(IpcError::Truncated);
        };
        self.pos = data_end;
        self.last_field_id = field_id;
        Ok(Some(Field { id: field_id, data }))
    }
}

/// Маркер-трейт протокола; его реализуют сгенерированные контракты.
pub trait Protocol {}

/// Handle клиентского конца канала с меткой протокола `P`.
/// В сообщении - сырой `u32`-идентификатор хэндла.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientEnd<P> {
    raw: NonZeroU32,
    _protocol: PhantomData<fn() -> P>,
}

impl<P> ClientEnd<P> {
    /// Оборачивает сырой handle-id в типизированный клиентский конец.
    #[must_use]
    pub const fn from_raw(raw: NonZeroU32) -> Self {
        Self {
            raw,
            _protocol: PhantomData,
        }
    }

    /// Сырой handle-id порта.
    #[must_use]
    pub const fn raw(self) -> NonZeroU32 {
        self.raw
    }
}

/// Handle серверного конца канала с меткой протокола `P`.
/// В сообщении - сырой `u32`-идентификатор хэндла.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerEnd<P> {
    raw: NonZeroU32,
    _protocol: PhantomData<fn() -> P>,
}

impl<P> ServerEnd<P> {
    /// Оборачивает сырой handle-id в типизированный серверный конец.
    #[must_use]
    pub const fn from_raw(raw: NonZeroU32) -> Self {
        Self {
            raw,
            _protocol: PhantomData,
        }
    }

    /// Сырой handle-id порта.
    #[must_use]
    pub const fn raw(self) -> NonZeroU32 {
        self.raw
    }
}

/// Нетипизированный capability: сырой `u32`-идентификатор хэндла в сообщении.
/// В теле едет индекс в handle-массиве; сам id - вне тела.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    raw: NonZeroU32,
}

impl Cap {
    /// Оборачивает сырой handle-id в capability.
    #[must_use]
    pub const fn from_raw(raw: NonZeroU32) -> Self {
        Self { raw }
    }

    /// Сырой handle-id capability.
    #[must_use]
    pub const fn raw(self) -> NonZeroU32 {
        self.raw
    }
}

/// Bounded UTF-8 строка: фактическая длина <= `N`.
/// `data` записи - UTF-8 байты без длины-префикса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Str<'a, const N: usize> {
    value: &'a str,
}

impl<'a, const N: usize> Str<'a, N> {
    /// Оборачивает строку, проверяя границу `N`.
    /// `BoundExceeded`, если байтовая длина превышает `N`.
    pub fn new(value: &'a str) -> Result<Self, IpcError> {
        if value.len() > N {
            return Err(IpcError::BoundExceeded);
        }
        Ok(Self { value })
    }

    /// Строковое значение.
    #[must_use]
    pub const fn as_str(self) -> &'a str {
        self.value
    }
}

/// Bounded байтовая строка: фактическая длина <= `N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bytes<'a, const N: usize> {
    value: &'a [u8],
}

impl<'a, const N: usize> Bytes<'a, N> {
    /// Оборачивает срез, проверяя границу `N`.
    /// `BoundExceeded`, если длина превышает `N`.
    pub fn new(value: &'a [u8]) -> Result<Self, IpcError> {
        if value.len() > N {
            return Err(IpcError::BoundExceeded);
        }
        Ok(Self { value })
    }

    /// Байтовое значение.
    #[must_use]
    pub const fn as_bytes(self) -> &'a [u8] {
        self.value
    }
}

/// Кодирование значений в `data` записи поля. Все целые - LE.
pub mod value {
    use super::{ENUM_DISCRIMINANT_SIZE, IpcError};

    macro_rules! int_codec {
        ($encode:ident, $decode:ident, $ty:ty) => {
            /// Кодирует целое LE в `data` записи; возвращает заполненный буфер.
            #[must_use]
            pub fn $encode(value: $ty) -> [u8; core::mem::size_of::<$ty>()] {
                value.to_le_bytes()
            }

            /// Декодирует целое LE из `data`; `BadLength` при неверном размере.
            pub fn $decode(data: &[u8]) -> Result<$ty, IpcError> {
                let bytes = data.try_into().map_err(|_| IpcError::BadLength)?;
                Ok(<$ty>::from_le_bytes(bytes))
            }
        };
    }

    int_codec!(encode_u8, decode_u8, u8);
    int_codec!(encode_u16, decode_u16, u16);
    int_codec!(encode_u32, decode_u32, u32);
    int_codec!(encode_u64, decode_u64, u64);
    int_codec!(encode_i8, decode_i8, i8);
    int_codec!(encode_i16, decode_i16, i16);
    int_codec!(encode_i32, decode_i32, i32);
    int_codec!(encode_i64, decode_i64, i64);

    /// Кодирует `bool` одним байтом (`0`/`1`).
    #[must_use]
    pub fn encode_bool(value: bool) -> [u8; 1] {
        [u8::from(value)]
    }

    /// Декодирует `bool` из одного байта; `BadBool` если не `0`/`1`.
    pub fn decode_bool(data: &[u8]) -> Result<bool, IpcError> {
        match data {
            [0] => Ok(false),
            [1] => Ok(true),
            [_] => Err(IpcError::BadBool),
            _ => Err(IpcError::BadLength),
        }
    }

    /// Кодирует дискриминант закрытого enum как `u16` LE.
    #[must_use]
    pub fn encode_discriminant(value: u16) -> [u8; ENUM_DISCRIMINANT_SIZE] {
        value.to_le_bytes()
    }

    /// Декодирует дискриминант `u16` LE; `BadLength` при неверном размере.
    pub fn decode_discriminant(data: &[u8]) -> Result<u16, IpcError> {
        let bytes = data.try_into().map_err(|_| IpcError::BadLength)?;
        Ok(u16::from_le_bytes(bytes))
    }

    /// Декодирует `data` как UTF-8; `NotUtf8` при некорректной кодировке.
    pub fn decode_str(data: &[u8]) -> Result<&str, IpcError> {
        core::str::from_utf8(data).map_err(|_| IpcError::NotUtf8)
    }

    /// Декодирует индекс порта из одного байта (`u8`).
    pub fn decode_port_index(data: &[u8]) -> Result<u8, IpcError> {
        match data {
            [index] => Ok(*index),
            _ => Err(IpcError::BadLength),
        }
    }

    /// Кодирует индекс порта одним байтом (`u8`).
    #[must_use]
    pub fn encode_port_index(index: u8) -> [u8; 1] {
        [index]
    }
}

#[cfg(test)]
mod tests {
    use super::{value::*, *};

    const ORDINAL: u64 = 0x0102_0304_0506_0708;
    const TXID: u32 = 0x1112_1314;

    #[test]
    fn header_golden_layout() {
        let header = Header::new(ORDINAL, TXID, FLAG_RESPONSE | FLAG_DOMAIN_ERR);
        let mut buf = [0u8; HEADER_SIZE];
        header.encode(&mut buf).expect("encode ok");
        // ordinal LE @0..8, txid LE @8..12, flags LE @12..14.
        assert_eq!(
            buf,
            [
                0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // ordinal
                0x14, 0x13, 0x12, 0x11, // txid
                0x09, 0x00, // flags = RESPONSE | DOMAIN_ERR = 0b1001
            ]
        );
    }

    #[test]
    fn header_round_trip() {
        let header = Header::new(ORDINAL, TXID, FLAG_FLEXIBLE);
        let mut buf = [0u8; HEADER_SIZE];
        header.encode(&mut buf).expect("encode ok");
        let decoded = Header::decode(&buf).expect("decode ok");
        assert_eq!(decoded, header);
        assert!(decoded.has_flag(FLAG_FLEXIBLE));
        assert!(!decoded.has_flag(FLAG_RESPONSE));
    }

    #[test]
    fn header_encode_rejects_short_buffer() {
        let header = Header::new(ORDINAL, TXID, 0);
        let mut buf = [0u8; HEADER_SIZE - 1];
        assert_eq!(header.encode(&mut buf), Err(IpcError::Truncated));
    }

    #[test]
    fn header_decode_rejects_short_buffer() {
        let bytes = [0u8; HEADER_SIZE - 1];
        assert_eq!(Header::decode(&bytes), Err(IpcError::Truncated));
    }

    #[test]
    fn derived_budget_constants() {
        assert_eq!(HEADER_SIZE, 14);
        assert_eq!(MESSAGE_INLINE_MAX, 256);
        assert_eq!(BODY_MAX, 242);
        assert_eq!(FIELD_DATA_MAX, 239);
        assert_eq!(MESSAGE_MAX_HANDLES, 4);
    }

    #[test]
    fn hello_body_golden_bytes() {
        // hello(version: u16 = 1): field_id=1, len=2, data=1u16 LE, терминатор.
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_field(1, &encode_u16(1)).expect("field ok");
        buf.finish().expect("finish ok");
        assert_eq!(buf.as_bytes(), &[0x01, 0x02, 0x01, 0x00, 0x00]);
    }

    #[test]
    fn full_frame_header_plus_body() {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_header(&Header::new(ORDINAL, TXID, 0))
            .expect("header ok");
        buf.write_field(1, &encode_u16(1)).expect("field ok");
        buf.finish().expect("finish ok");
        assert_eq!(buf.len(), HEADER_SIZE + 5);
        // Тело после заголовка совпадает с golden.
        assert_eq!(
            &buf.as_bytes()[HEADER_SIZE..],
            &[0x01, 0x02, 0x01, 0x00, 0x00]
        );
    }

    #[test]
    fn int_codecs_round_trip() {
        assert_eq!(decode_u8(&encode_u8(0xAB)), Ok(0xAB));
        assert_eq!(decode_u16(&encode_u16(0xABCD)), Ok(0xABCD));
        assert_eq!(decode_u32(&encode_u32(0x0123_4567)), Ok(0x0123_4567));
        assert_eq!(
            decode_u64(&encode_u64(0x0123_4567_89AB_CDEF)),
            Ok(0x0123_4567_89AB_CDEF)
        );
        assert_eq!(decode_i8(&encode_i8(-5)), Ok(-5));
        assert_eq!(decode_i16(&encode_i16(-300)), Ok(-300));
        assert_eq!(decode_i32(&encode_i32(-70_000)), Ok(-70_000));
        assert_eq!(decode_i64(&encode_i64(-5_000_000_000)), Ok(-5_000_000_000));
    }

    #[test]
    fn int_codec_le_byte_order() {
        assert_eq!(encode_u32(0x0102_0304), [0x04, 0x03, 0x02, 0x01]);
    }

    #[test]
    fn int_codec_rejects_wrong_length() {
        assert_eq!(decode_u32(&[0, 0, 0]), Err(IpcError::BadLength));
        assert_eq!(decode_u32(&[0, 0, 0, 0, 0]), Err(IpcError::BadLength));
    }

    #[test]
    fn bool_codec_round_trip() {
        assert_eq!(encode_bool(true), [1]);
        assert_eq!(encode_bool(false), [0]);
        assert_eq!(decode_bool(&[1]), Ok(true));
        assert_eq!(decode_bool(&[0]), Ok(false));
        assert_eq!(decode_bool(&[2]), Err(IpcError::BadBool));
        assert_eq!(decode_bool(&[]), Err(IpcError::BadLength));
    }

    #[test]
    fn discriminant_codec_round_trip() {
        assert_eq!(encode_discriminant(7), [7, 0]);
        assert_eq!(
            decode_discriminant(&encode_discriminant(0xBEEF)),
            Ok(0xBEEF)
        );
        assert_eq!(decode_discriminant(&[1]), Err(IpcError::BadLength));
    }

    #[test]
    fn port_index_codec_round_trip() {
        assert_eq!(encode_port_index(3), [3]);
        assert_eq!(decode_port_index(&encode_port_index(3)), Ok(3));
        assert_eq!(decode_port_index(&[1, 2]), Err(IpcError::BadLength));
    }

    #[test]
    fn str_round_trip_within_bound() {
        let s = Str::<8>::new("hello").expect("within bound");
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_field(1, s.as_str().as_bytes()).expect("field ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("ok").expect("present");
        assert_eq!(field.id, 1);
        assert_eq!(decode_str(field.data), Ok("hello"));
    }

    #[test]
    fn str_rejects_over_bound() {
        assert_eq!(Str::<3>::new("abcd"), Err(IpcError::BoundExceeded));
    }

    #[test]
    fn str_accepts_exact_bound() {
        assert!(Str::<4>::new("abcd").is_ok());
    }

    #[test]
    fn bytes_round_trip_within_bound() {
        let b = Bytes::<8>::new(&[1, 2, 3]).expect("within bound");
        assert_eq!(b.as_bytes(), &[1, 2, 3]);
    }

    #[test]
    fn bytes_rejects_over_bound() {
        assert_eq!(Bytes::<2>::new(&[1, 2, 3]), Err(IpcError::BoundExceeded));
    }

    #[test]
    fn decode_str_rejects_non_utf8() {
        assert_eq!(decode_str(&[0xFF, 0xFE]), Err(IpcError::NotUtf8));
    }

    #[test]
    fn skip_unknown_field() {
        // Тело: известное поле id=1, неизвестное id=5, известное id=9.
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_field(1, &encode_u16(0xAAAA)).expect("ok");
        buf.write_field(5, &encode_u32(0xDEAD_BEEF)).expect("ok");
        buf.write_field(9, &encode_u8(0x42)).expect("ok");
        buf.finish().expect("ok");

        // Декодер знает только id=1 и id=9, id=5 пропускает по len.
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let mut seen_1 = None;
        let mut seen_9 = None;
        while let Some(field) = cursor.next_field().expect("no decode error") {
            match field.id {
                1 => seen_1 = Some(decode_u16(field.data).expect("u16")),
                9 => seen_9 = Some(decode_u8(field.data).expect("u8")),
                _ => {} // неизвестное поле уже пропущено курсором по len
            }
        }
        assert_eq!(seen_1, Some(0xAAAA));
        assert_eq!(seen_9, Some(0x42));
    }

    #[test]
    fn missing_field_is_absent() {
        // Тело с единственным полем id=1; запрошенного id=2 нет - absent.
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_field(1, &encode_u16(1)).expect("ok");
        buf.finish().expect("ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let mut found_2 = false;
        while let Some(field) = cursor.next_field().expect("ok") {
            if field.id == 2 {
                found_2 = true;
            }
        }
        assert!(!found_2);
    }

    #[test]
    fn frame_budget_overflow() {
        // Буфер ровно под MESSAGE_INLINE_MAX; поле сверх бюджета - ошибка, не паника.
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_header(&Header::new(ORDINAL, TXID, 0))
            .expect("header ok");
        // FIELD_DATA_MAX данных ровно заполняют кадр с терминатором.
        let max_data = [0u8; FIELD_DATA_MAX];
        buf.write_field(1, &max_data).expect("max field fits");
        // Ещё один байт данных уже не влезает.
        assert_eq!(buf.write_field(2, &[0u8]), Err(IpcError::FrameOverflow));
    }

    #[test]
    fn single_field_fills_frame_exactly() {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_header(&Header::new(ORDINAL, TXID, 0))
            .expect("header ok");
        buf.write_field(1, &[0u8; FIELD_DATA_MAX]).expect("fits");
        buf.finish().expect("finish ok");
        assert_eq!(buf.len(), MESSAGE_INLINE_MAX);
    }

    #[test]
    fn field_order_strictly_increasing() {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_field(3, &encode_u8(1)).expect("ok");
        // Равный id отвергается.
        assert_eq!(buf.write_field(3, &encode_u8(2)), Err(IpcError::FieldOrder));
        // Меньший id отвергается.
        assert_eq!(buf.write_field(2, &encode_u8(2)), Err(IpcError::FieldOrder));
        // Больший id принимается.
        assert!(buf.write_field(4, &encode_u8(2)).is_ok());
    }

    #[test]
    fn field_id_zero_and_reserved_rejected() {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        assert_eq!(buf.write_field(0, &[1]), Err(IpcError::FieldOrder));
        assert_eq!(buf.write_field(255, &[1]), Err(IpcError::FieldOrder));
    }

    #[test]
    fn reader_rejects_truncated_len() {
        // Запись заявляет len=4, но данных нет - Truncated, не паника.
        let body = [0x01u8, 0x04, 0x00];
        let mut cursor = FieldCursor::new(&body);
        assert_eq!(cursor.next_field(), Err(IpcError::Truncated));
    }

    #[test]
    fn reader_rejects_non_increasing_field_id() {
        // Тело с id=2 затем id=1 - нарушение порядка на чтении.
        let body = [0x02u8, 0x01, 0xAA, 0x01, 0x01, 0xBB, 0x00];
        let mut cursor = FieldCursor::new(&body);
        assert_eq!(
            cursor.next_field().expect("first ok").map(|f| f.id),
            Some(2)
        );
        assert_eq!(cursor.next_field(), Err(IpcError::FieldOrder));
    }

    #[test]
    fn reader_stops_on_terminator() {
        let body = [0x01u8, 0x01, 0xAA, 0x00];
        let mut cursor = FieldCursor::new(&body);
        assert_eq!(cursor.next_field().expect("ok").map(|f| f.id), Some(1));
        assert_eq!(cursor.next_field().expect("ok"), None);
        // Повторный вызов после терминатора - всё ещё None.
        assert_eq!(cursor.next_field().expect("ok"), None);
    }

    #[test]
    fn write_header_only_first() {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_field(1, &encode_u8(1)).expect("ok");
        // Заголовок после поля недопустим.
        assert_eq!(
            buf.write_header(&Header::new(ORDINAL, TXID, 0)),
            Err(IpcError::FieldOrder)
        );
    }

    #[test]
    fn finish_is_idempotent() {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_field(1, &encode_u8(1)).expect("ok");
        buf.finish().expect("first finish");
        let len_after_first = buf.len();
        buf.finish().expect("second finish");
        assert_eq!(buf.len(), len_after_first);
    }

    #[test]
    fn write_after_finish_rejected() {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        buf.write_field(1, &encode_u8(1)).expect("ok");
        buf.finish().expect("finish");
        assert_eq!(buf.write_field(2, &encode_u8(2)), Err(IpcError::FieldOrder));
    }

    #[test]
    fn port_repr_transparent_size() {
        // repr(transparent) над NonZeroU32: размер 4, niche-оптимизация Option.
        assert_eq!(core::mem::size_of::<ClientEnd<()>>(), 4);
        assert_eq!(core::mem::size_of::<ServerEnd<()>>(), 4);
        assert_eq!(core::mem::size_of::<Option<ClientEnd<()>>>(), 4);
    }

    #[test]
    fn port_from_raw_round_trip() {
        let raw = NonZeroU32::new(7).expect("nonzero");
        let client = ClientEnd::<()>::from_raw(raw);
        let server = ServerEnd::<()>::from_raw(raw);
        assert_eq!(client.raw(), raw);
        assert_eq!(server.raw(), raw);
    }
}
