//! Wire-формат сообщения: заголовок, тело (table-of-fields), кодек значений,
//! стековый буфер кадра и слоты capability.
//!
//! Все целые - little-endian. Кадр (заголовок + тело) укладывается в
//! [`MESSAGE_INLINE_MAX`]. Кодирование без heap.

use core::{marker::PhantomData, num::NonZeroU32};

use syscall::{Handle, ObjectKind, kind};

pub use crate::schema::{FIELD_OVERHEAD, HEADER_SIZE};
use crate::{
    Channel, IntoWireValue, Protocol, ReceivedResources, WireTyped, WireValue, schema::WireType,
};

/// Максимальная длина кадра (заголовок + тело) в байтах. Кадр заполняет тело IPC-буфера.
/// Это значение равно [`syscall::IPC_BUFFER_DATA_MAX`].
pub const MESSAGE_INLINE_MAX: usize = syscall::IPC_BUFFER_DATA_MAX;

/// Максимум хэндлов в транспортном векторе кадра.
pub const MESSAGE_MAX_HANDLES: usize = 4;

const _: () = assert!(
    syscall::IPC_BUFFER_MAX_CAPS <= MESSAGE_MAX_HANDLES,
    "every handle the kernel delivers with a frame must fit into the slots"
);

/// Бюджет тела (включая терминатор): `MESSAGE_INLINE_MAX - HEADER_SIZE`.
pub const BODY_MAX: usize = MESSAGE_INLINE_MAX - HEADER_SIZE;

/// Максимум `data` одиночного поля, занимающего кадр целиком:
/// `BODY_MAX - FIELD_OVERHEAD - 1` (терминатор). Длина записи хранится как `u16`
/// LE, и `FIELD_DATA_MAX` укладывается в диапазон `u16`.
pub const FIELD_DATA_MAX: usize = BODY_MAX - FIELD_OVERHEAD - 1;

const _: () = assert!(FIELD_DATA_MAX <= u16::MAX as usize);

/// Байт-терминатор тела. `field_id == 0` зарезервирован под него.
pub const TERMINATOR: u8 = 0x00;

/// Минимальный допустимый `field_id` записи.
pub const FIELD_ID_MIN: u8 = 1;

/// Максимальный допустимый `field_id` записи.
pub const FIELD_ID_MAX: u8 = 254;

/// Ширина дискриминанта закрытого enum как поля wire-типа.
pub const ENUM_DISCRIMINANT_WIDTH: u8 = 2;

/// Размер дискриминанта закрытого enum в сообщении (`u16` LE).
pub const ENUM_DISCRIMINANT_SIZE: usize = ENUM_DISCRIMINANT_WIDTH as usize;

/// Ошибка кодека wire-формата и транспорта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    /// Буфер усечён. Для чтения/записи не хватает байт.
    Truncated,
    /// Запись/кадр превысили бюджет [`MESSAGE_INLINE_MAX`].
    FrameOverflow,
    /// `field_id` вне диапазона `1..=254` либо нарушает возрастание.
    FieldOrder,
    /// Обязательное поле отсутствует в теле.
    MissingField,
    /// Значение поля не декодируется как ожидаемый wire-тип.
    InvalidValue,
    /// Дальний конец канала закрыт.
    PeerClosed,
    /// Истёк отведённый на ожидание срок.
    Timeout,
    /// Капы сообщения не умещаются в [`MESSAGE_MAX_HANDLES`] слотов.
    TooManyHandles,
    /// Ресурс приёмной стороны исчерпан: кадр доставлен, но принять его нечем.
    ResourceExhausted,
}

/// Заголовок кадра: ординал операции, `u64` LE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Идентификатор операции (хэш канонического имени).
    pub ordinal: u64,
}

impl Header {
    /// Создаёт заголовок операции `ordinal`.
    #[must_use]
    pub const fn new(ordinal: u64) -> Self {
        Self { ordinal }
    }

    /// Записывает заголовок в первые [`HEADER_SIZE`] байт `buf`.
    /// `Truncated`, если буфер короче заголовка.
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), IpcError> {
        let Some(ordinal) = buf.first_chunk_mut::<HEADER_SIZE>() else {
            return Err(IpcError::Truncated);
        };
        *ordinal = self.ordinal.to_le_bytes();
        Ok(())
    }

    /// Разделяет кадр на заголовок и тело. `Truncated`, если кадр короче заголовка.
    pub fn split(frame: &[u8]) -> Result<FrameParts<'_>, IpcError> {
        let Some((ordinal, body)) = frame.split_first_chunk::<HEADER_SIZE>() else {
            return Err(IpcError::Truncated);
        };
        Ok(FrameParts {
            header: Self {
                ordinal: u64::from_le_bytes(*ordinal),
            },
            body,
        })
    }
}

/// Кадр, разделённый на заголовок и тело.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameParts<'a> {
    /// Заголовок кадра.
    pub header: Header,
    /// Тело кадра: записи полей и терминатор.
    pub body: &'a [u8],
}

/// Стековый буфер исходящего кадра: байты кадра и слоты его capability.
///
/// Кадр строится как заголовок, затем записи полей, затем терминатор. Кадр, запись которого
/// отказала, не отправляется.
#[derive(Debug, Clone)]
pub struct MessageBuf {
    bytes: [u8; MESSAGE_INLINE_MAX],
    pos: usize,
    last_field_id: u8,
    open_records: usize,
    terminated: bool,
    handles: [u32; MESSAGE_MAX_HANDLES],
    handle_count: usize,
}

impl Default for MessageBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageBuf {
    /// Создаёт пустой буфер с нулевой позицией записи.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: [0u8; MESSAGE_INLINE_MAX],
            pos: 0,
            last_field_id: 0,
            open_records: 0,
            terminated: false,
            handles: [0; MESSAGE_MAX_HANDLES],
            handle_count: 0,
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
        self.bytes.get(..self.pos).unwrap_or_default()
    }

    /// Handle-вектор кадра: хэндлы, чьи индексы записаны в теле, в порядке записи.
    #[must_use]
    pub fn handles(&self) -> &[u32] {
        self.handles.get(..self.handle_count).unwrap_or_default()
    }

    /// Записывает заголовок в начало буфера. Продвигает позицию на
    /// [`HEADER_SIZE`]. Допустим только как первая запись.
    pub fn write_header(&mut self, header: &Header) -> Result<(), IpcError> {
        if self.pos != 0 {
            return Err(IpcError::FieldOrder);
        }
        header.encode(&mut self.bytes)?;
        self.pos = HEADER_SIZE;
        Ok(())
    }

    /// Записывает запись поля `field_id` с телом `data`.
    /// Держит строго возрастающий `field_id` в диапазоне `1..=254`. Нарушение
    /// порядка, `field_id` вне диапазона и запись после [`MessageBuf::finish`] -
    /// `FieldOrder`. `data` длиннее [`FIELD_DATA_MAX`] - `InvalidValue`.
    /// Превышение бюджета кадра - `FrameOverflow`.
    pub fn write_field(&mut self, field_id: u8, data: &[u8]) -> Result<(), IpcError> {
        self.check_field_id(field_id)?;
        if data.len() > FIELD_DATA_MAX {
            return Err(IpcError::InvalidValue);
        }
        let start = self.pos;
        let end = start + FIELD_OVERHEAD + data.len();
        self.check_room(end)?;
        self.bytes
            .get_mut(start + FIELD_OVERHEAD..end)
            .ok_or(IpcError::FrameOverflow)?
            .copy_from_slice(data);
        self.close_record(start, field_id, end)
    }

    /// Записывает поле-capability: хэндл `raw` занимает слот кадра, а тело записи - индекс
    /// этого слота. Сверх [`MESSAGE_MAX_HANDLES`] хэндлов - `TooManyHandles`.
    pub fn write_handle(&mut self, field_id: u8, raw: u32) -> Result<(), IpcError> {
        let index = self.handle_count;
        let slot_index = u8::try_from(index).map_err(|_| IpcError::TooManyHandles)?;
        if index >= MESSAGE_MAX_HANDLES {
            return Err(IpcError::TooManyHandles);
        }
        self.write_field(field_id, &[slot_index])?;
        if let Some(slot) = self.handles.get_mut(index) {
            *slot = raw;
        }
        self.handle_count += 1;
        Ok(())
    }

    /// Записывает поле `field_id`, тело которого - вложенные записи от `write` с собственным
    /// терминатором. Вложенные записи нумеруют `field_id` заново и делят слоты capability
    /// кадра. Ограничения `field_id`, длины и бюджета те же, что у [`MessageBuf::write_field`].
    pub fn write_nested(
        &mut self,
        field_id: u8,
        write: impl FnOnce(&mut Self) -> Result<(), IpcError>,
    ) -> Result<(), IpcError> {
        self.check_field_id(field_id)?;
        let start = self.pos;
        // Заголовок записи и её собственный терминатор.
        self.check_room(start + FIELD_OVERHEAD + 1)?;
        self.pos = start + FIELD_OVERHEAD;
        self.last_field_id = 0;
        self.open_records += 1;
        let written = write(self);
        self.open_records -= 1;
        written?;

        let terminator = self
            .bytes
            .get_mut(self.pos)
            .ok_or(IpcError::FrameOverflow)?;
        *terminator = TERMINATOR;
        let end = self.pos + 1;
        if end - start - FIELD_OVERHEAD > FIELD_DATA_MAX {
            return Err(IpcError::InvalidValue);
        }
        self.close_record(start, field_id, end)
    }

    /// Дописывает терминатор `0x00` и завершает тело. Идемпотентен. Внутри
    /// [`MessageBuf::write_nested`] - `FieldOrder`.
    pub fn finish(&mut self) -> Result<(), IpcError> {
        if self.terminated {
            return Ok(());
        }
        if self.open_records != 0 {
            return Err(IpcError::FieldOrder);
        }
        let slot = self
            .bytes
            .get_mut(self.pos)
            .ok_or(IpcError::FrameOverflow)?;
        *slot = TERMINATOR;
        self.pos += 1;
        self.terminated = true;
        Ok(())
    }

    fn check_field_id(&self, field_id: u8) -> Result<(), IpcError> {
        if self.terminated
            || !(FIELD_ID_MIN..=FIELD_ID_MAX).contains(&field_id)
            || field_id <= self.last_field_id
        {
            return Err(IpcError::FieldOrder);
        }
        Ok(())
    }

    fn check_room(&self, end: usize) -> Result<(), IpcError> {
        // За записью остаётся место под терминатор кадра и терминатор каждой открытой
        // вложенной записи.
        if end + 1 + self.open_records > MESSAGE_INLINE_MAX {
            return Err(IpcError::FrameOverflow);
        }
        Ok(())
    }

    fn close_record(&mut self, start: usize, field_id: u8, end: usize) -> Result<(), IpcError> {
        let data_len =
            u16::try_from(end - start - FIELD_OVERHEAD).map_err(|_| IpcError::FrameOverflow)?;
        let Some((id_slot, rest)) = self
            .bytes
            .get_mut(start..)
            .and_then(<[u8]>::split_first_mut)
        else {
            return Err(IpcError::FrameOverflow);
        };
        let Some(len_slot) = rest.first_chunk_mut::<2>() else {
            return Err(IpcError::FrameOverflow);
        };
        *id_slot = field_id;
        *len_slot = data_len.to_le_bytes();
        self.pos = end;
        self.last_field_id = field_id;
        Ok(())
    }
}

/// Курсор последовательного чтения тела итерирует записи полей.
/// Запись с неизвестным `field_id` курсор проходит по её длине и отдаёт
/// вызывающему наравне с прочими. Разбор такой записи вызывающий пропускает.
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
        let tail = self.body.get(self.pos..).unwrap_or_default();
        let Some((&field_id, after_id)) = tail.split_first() else {
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
        let Some((len_bytes, after_len)) = after_id.split_first_chunk::<2>() else {
            return Err(IpcError::Truncated);
        };
        let len = u16::from_le_bytes(*len_bytes) as usize;
        let Some((data, _)) = after_len.split_at_checked(len) else {
            return Err(IpcError::Truncated);
        };
        self.pos += FIELD_OVERHEAD + len;
        self.last_field_id = field_id;
        Ok(Some(Field { id: field_id, data }))
    }
}

/// Поле-capability кадра, помеченное видом объекта `K`. В теле передаётся индекс в
/// handle-массиве. Сам id - вне тела.
///
/// Вид задаётся объявлением поля контракта и в кадр не записывается. Значение хэндлом не
/// владеет. Закрытие принятого хэндла остаётся за
/// [`ReceivedResources`] кадра, а реализация
/// [`relinquish`](crate::IntoWireValue::relinquish) для `Cap` пуста.
#[repr(transparent)]
pub struct Cap<K: ObjectKind> {
    handle: Handle<K>,
}

impl<K: ObjectKind> Cap<K> {
    /// Помечает уже полученный handle как поле кадра. Вид сохраняется.
    #[must_use]
    pub const fn from_handle(handle: Handle<K>) -> Self {
        Self { handle }
    }

    /// Handle поля вместе с его видом.
    #[must_use]
    pub const fn as_handle(self) -> Handle<K> {
        self.handle
    }

    /// Сырой handle-id capability.
    #[must_use]
    pub const fn raw(self) -> NonZeroU32 {
        self.handle.as_nonzero()
    }

    /// Снимает статическую метку вида, сохраняя идентификатор.
    #[must_use]
    pub const fn erase(self) -> Cap<kind::Any> {
        Cap::from_handle(self.handle.erase())
    }
}

impl<K: ObjectKind> Clone for Cap<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K: ObjectKind> Copy for Cap<K> {}

impl<K: ObjectKind> PartialEq for Cap<K> {
    fn eq(&self, other: &Self) -> bool {
        self.handle == other.handle
    }
}

impl<K: ObjectKind> Eq for Cap<K> {}

impl<K: ObjectKind> core::fmt::Debug for Cap<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("Cap").field(&self.handle).finish()
    }
}

/// Сессия протокола `P`. Запись по умолчанию содержит capability порта, который обслуживает `P`,
/// и хэндлом не владеет. Принятая сессия владеет хэндлом и сохраняет протокол.
///
/// `H` - держатель capability порта. В кадре сессия записана так же, как [`Cap<kind::Port>`],
/// а протокол существует только при компиляции. Получатель читает запись сессией того же `P`
/// с хэндлом из ресурсов кадра, и эту сессию можно отправить дальше владеющим значением.
pub struct Session<P: Protocol, H = Cap<kind::Port>> {
    handle: H,
    protocol: PhantomData<fn() -> P>,
}

/// Держатель порта сессии, из которого строится канал клиента.
pub trait SessionHandle {
    /// Канал клиента поверх порта.
    type Channel: Channel;

    /// Канал поверх порта. Порт переходит каналу.
    fn into_channel(self) -> Self::Channel;
}

impl<P: Protocol, H> Session<P, H> {
    /// Сессия поверх хэндла порта. Вызывающий утверждает, что порт обслуживает `P`.
    #[must_use]
    pub const fn from_handle(handle: H) -> Self {
        Self {
            handle,
            protocol: PhantomData,
        }
    }

    /// Та же сессия с другим держателем порта. Протокол переходит без повторного утверждения.
    #[must_use]
    pub fn from_session<F>(session: Session<P, F>) -> Self
    where
        H: From<F>,
    {
        Self::from_handle(H::from(session.handle))
    }

    /// Держатель порта сессии.
    #[must_use]
    pub const fn handle(&self) -> &H {
        &self.handle
    }

    /// Держатель порта сессии без протокола.
    #[must_use]
    pub fn into_handle(self) -> H {
        self.handle
    }
}

impl<P: Protocol, H: SessionHandle> Session<P, H> {
    /// Клиент протокола `P`. Порт сессии переходит каналу клиента.
    pub fn client(self) -> P::Client<H::Channel> {
        P::client(self.handle.into_channel())
    }
}

impl<P: Protocol> WireTyped for Session<P> {
    const WIRE_TYPE: WireType = <Cap<kind::Port> as WireTyped>::WIRE_TYPE;
}

impl<'a, P: Protocol, R: ReceivedResources> WireValue<'a, R> for Session<P> {
    type Owned = Session<P, R::Handle<kind::Port>>;

    fn from_field(data: &'a [u8], frame: &mut R) -> Result<Self::Owned, IpcError> {
        let handle = <Cap<kind::Port> as WireValue<'a, R>>::from_field(data, frame)?;
        Ok(Session {
            handle,
            protocol: PhantomData,
        })
    }
}

impl<P: Protocol, H: IntoWireValue<Read = Cap<kind::Port>>> IntoWireValue for Session<P, H> {
    type Read = Session<P>;

    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
        self.handle.write_as_field(buf, field_id)
    }

    fn relinquish(self) {
        self.handle.relinquish();
    }
}

impl<P: Protocol, H: Clone> Clone for Session<P, H> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            protocol: PhantomData,
        }
    }
}

impl<P: Protocol, H: Copy> Copy for Session<P, H> {}

impl<P: Protocol, H: PartialEq> PartialEq for Session<P, H> {
    fn eq(&self, other: &Self) -> bool {
        self.handle == other.handle
    }
}

impl<P: Protocol, H: Eq> Eq for Session<P, H> {}

impl<P: Protocol, H: core::fmt::Debug> core::fmt::Debug for Session<P, H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Session")
            .field("protocol", &P::ID)
            .field("handle", &self.handle)
            .finish()
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
    /// `None`, если байтовая длина превышает `N`.
    pub fn new(value: &'a str) -> Option<Self> {
        if value.len() > N {
            return None;
        }
        Some(Self { value })
    }

    /// Префикс `value` длиной не более `N` байт, обрезанный по границе символа.
    #[must_use]
    pub fn truncating(value: &'a str) -> Self {
        if value.len() <= N {
            return Self { value };
        }
        let mut len = N;
        while len > 0 && !value.is_char_boundary(len) {
            len -= 1;
        }
        Self {
            value: value.split_at_checked(len).map_or("", |(head, _)| head),
        }
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
    /// `None`, если длина превышает `N`.
    pub fn new(value: &'a [u8]) -> Option<Self> {
        if value.len() > N {
            return None;
        }
        Some(Self { value })
    }

    /// Байтовое значение.
    #[must_use]
    pub const fn as_bytes(self) -> &'a [u8] {
        self.value
    }
}

/// [`Str`] с границей [`FIELD_DATA_MAX`] - максимум для одного поля.
pub type FieldStr<'a> = Str<'a, FIELD_DATA_MAX>;

/// [`Bytes`] с границей [`FIELD_DATA_MAX`] - максимум для одного поля.
pub type FieldBytes<'a> = Bytes<'a, FIELD_DATA_MAX>;

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

            /// Декодирует целое LE из `data`; `InvalidValue` при неверном размере.
            pub fn $decode(data: &[u8]) -> Result<$ty, IpcError> {
                let bytes = data.try_into().map_err(|_| IpcError::InvalidValue)?;
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

    /// Декодирует `bool` из одного байта. `InvalidValue` при неверном байте или размере.
    pub fn decode_bool(data: &[u8]) -> Result<bool, IpcError> {
        match data {
            [0] => Ok(false),
            [1] => Ok(true),
            _ => Err(IpcError::InvalidValue),
        }
    }

    /// Кодирует дискриминант закрытого enum как `u16` LE.
    #[must_use]
    pub fn encode_discriminant(value: u16) -> [u8; ENUM_DISCRIMINANT_SIZE] {
        value.to_le_bytes()
    }

    /// Декодирует дискриминант `u16` LE. `InvalidValue` при неверном размере.
    pub fn decode_discriminant(data: &[u8]) -> Result<u16, IpcError> {
        let bytes = data.try_into().map_err(|_| IpcError::InvalidValue)?;
        Ok(u16::from_le_bytes(bytes))
    }

    /// Декодирует `data` как UTF-8. `InvalidValue` при некорректной кодировке.
    pub fn decode_str(data: &[u8]) -> Result<&str, IpcError> {
        core::str::from_utf8(data).map_err(|_| IpcError::InvalidValue)
    }

    /// Декодирует индекс порта из одного байта (`u8`).
    pub fn decode_port_index(data: &[u8]) -> Result<u8, IpcError> {
        match data {
            [index] => Ok(*index),
            _ => Err(IpcError::InvalidValue),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{value::*, *};

    const ORDINAL: u64 = 0x0102_0304_0506_0708;

    fn framed() -> MessageBuf {
        let mut buf = MessageBuf::new();
        buf.write_header(&Header::new(ORDINAL)).expect("header ok");
        buf
    }

    fn first_field(body: &[u8]) -> Result<Option<Field<'_>>, IpcError> {
        FieldCursor::new(body).next_field()
    }

    #[test]
    fn header_golden_layout() {
        let mut buf = [0u8; HEADER_SIZE + 1];
        Header::new(ORDINAL).encode(&mut buf).expect("encode ok");
        buf[HEADER_SIZE] = 0xEE;
        // ordinal LE @0..8, дальше тело.
        assert_eq!(buf, [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0xEE]);
        assert_eq!(
            Header::split(&buf),
            Ok(FrameParts {
                header: Header::new(ORDINAL),
                body: &[0xEE],
            })
        );
    }

    #[test]
    fn header_shorter_than_its_size_is_refused() {
        assert_eq!(
            Header::new(ORDINAL).encode(&mut [0u8; HEADER_SIZE - 1]),
            Err(IpcError::Truncated)
        );
        assert_eq!(
            Header::split(&[0u8; HEADER_SIZE - 1]),
            Err(IpcError::Truncated)
        );
    }

    #[test]
    fn derived_budget_constants() {
        assert_eq!(HEADER_SIZE, 8);
        assert_eq!(MESSAGE_INLINE_MAX, 4064);
        assert_eq!(BODY_MAX, 4056);
        assert_eq!(FIELD_DATA_MAX, 4052);
        assert_eq!(MESSAGE_MAX_HANDLES, 4);
    }

    #[test]
    fn hello_body_golden_bytes() {
        // hello(version: u16 = 1): field_id=1, len=2u16 LE, data=1u16 LE, терминатор.
        let mut buf = MessageBuf::new();
        buf.write_field(1, &encode_u16(1)).expect("field ok");
        buf.finish().expect("finish ok");
        assert_eq!(buf.as_bytes(), &[0x01, 0x02, 0x00, 0x01, 0x00, 0x00]);
    }

    #[test]
    fn value_decoders_refuse_a_wrong_encoding() {
        assert_eq!(decode_u32(&[0, 0, 0]), Err(IpcError::InvalidValue));
        assert_eq!(decode_u32(&[0, 0, 0, 0, 0]), Err(IpcError::InvalidValue));
        assert_eq!(decode_bool(&[2]), Err(IpcError::InvalidValue));
        assert_eq!(decode_bool(&[]), Err(IpcError::InvalidValue));
        assert_eq!(decode_str(&[0xFF, 0xFE]), Err(IpcError::InvalidValue));
    }

    #[test]
    fn bounded_values_admit_their_bound_and_refuse_more() {
        assert!(Str::<4>::new("abcd").is_some());
        assert!(Str::<3>::new("abcd").is_none());
        assert!(Bytes::<3>::new(&[1, 2, 3]).is_some());
        assert!(Bytes::<2>::new(&[1, 2, 3]).is_none());
    }

    #[test]
    fn cursor_passes_over_an_unknown_field() {
        // Тело: известное поле id=1, неизвестное id=5, известное id=9.
        let mut buf = MessageBuf::new();
        buf.write_field(1, &encode_u16(0xAAAA)).expect("ok");
        buf.write_field(5, &encode_u32(0xDEAD_BEEF)).expect("ok");
        buf.write_field(9, &encode_u8(0x42)).expect("ok");
        buf.finish().expect("ok");

        let mut cursor = FieldCursor::new(buf.as_bytes());
        let ids: [u8; 3] =
            core::array::from_fn(|_| cursor.next_field().expect("ok").expect("present").id);
        assert_eq!(ids, [1, 5, 9]);
        assert_eq!(cursor.next_field(), Ok(None));
    }

    #[test]
    fn single_field_fills_the_frame_and_one_byte_more_overflows() {
        let mut exact = framed();
        exact.write_field(1, &[0u8; FIELD_DATA_MAX]).expect("fits");
        exact.finish().expect("finish ok");
        assert_eq!(exact.len(), MESSAGE_INLINE_MAX);

        let mut over = framed();
        over.write_field(1, &[0u8; FIELD_DATA_MAX]).expect("fits");
        assert_eq!(over.write_field(2, &[0u8]), Err(IpcError::FrameOverflow));
        assert_eq!(
            MessageBuf::new().write_field(1, &[0u8; FIELD_DATA_MAX + 1]),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn field_longer_than_255_bytes_round_trips() {
        const LEN: usize = 1500;
        // Длина записи занимает u16, и данные длиннее u8::MAX не усекаются.
        let payload: [u8; LEN] = core::array::from_fn(|i| (i % 251) as u8);
        let mut buf = MessageBuf::new();
        buf.write_field(7, &payload).expect("field fits");
        buf.finish().expect("finish ok");

        let field = first_field(buf.as_bytes()).expect("ok").expect("present");
        assert_eq!((field.id, field.data), (7, &payload[..]));
    }

    #[test]
    fn writer_refuses_field_ids_out_of_order_or_range() {
        let mut buf = MessageBuf::new();
        buf.write_field(3, &[1]).expect("ok");
        assert_eq!(buf.write_field(3, &[2]), Err(IpcError::FieldOrder));
        assert_eq!(buf.write_field(2, &[2]), Err(IpcError::FieldOrder));
        assert_eq!(buf.write_field(0, &[1]), Err(IpcError::FieldOrder));
        assert_eq!(buf.write_field(255, &[1]), Err(IpcError::FieldOrder));
        assert!(buf.write_field(4, &[2]).is_ok());
    }

    #[test]
    fn writer_refuses_a_header_after_a_field_and_a_field_after_finish() {
        let mut buf = MessageBuf::new();
        buf.write_field(1, &[1]).expect("ok");
        assert_eq!(
            buf.write_header(&Header::new(ORDINAL)),
            Err(IpcError::FieldOrder)
        );
        buf.finish().expect("finish");
        assert_eq!(buf.write_field(2, &[2]), Err(IpcError::FieldOrder));
    }

    #[test]
    fn reader_refuses_a_truncated_record_and_a_non_increasing_field_id() {
        // Запись заявляет len=4 без данных.
        assert_eq!(first_field(&[0x01, 0x04, 0x00]), Err(IpcError::Truncated));

        // id=2, затем id=1.
        let body = [0x02u8, 0x01, 0x00, 0xAA, 0x01, 0x01, 0x00, 0xBB, 0x00];
        let mut cursor = FieldCursor::new(&body);
        assert_eq!(
            cursor.next_field().expect("first ok").map(|f| f.id),
            Some(2)
        );
        assert_eq!(cursor.next_field(), Err(IpcError::FieldOrder));
    }

    #[test]
    fn nested_record_golden_bytes() {
        // Поле 1 с вложенной записью: поле 1 = 0xAA и терминатор вложенного тела.
        let mut buf = MessageBuf::new();
        buf.write_nested(1, |inner| inner.write_field(1, &[0xAA]))
            .expect("nested ok");
        buf.finish().expect("finish ok");
        assert_eq!(
            buf.as_bytes(),
            &[0x01, 0x05, 0x00, 0x01, 0x01, 0x00, 0xAA, 0x00, 0x00]
        );
    }

    #[test]
    fn nested_record_numbers_its_fields_anew_and_keeps_the_outer_order() {
        let mut buf = MessageBuf::new();
        buf.write_field(1, &[1]).expect("outer 1");
        buf.write_nested(2, |inner| inner.write_field(1, &[2]))
            .expect("nested 2");
        assert_eq!(buf.write_field(2, &[3]), Err(IpcError::FieldOrder));
        assert!(buf.write_field(3, &[3]).is_ok());
    }

    #[test]
    fn nested_record_reserves_every_open_terminator() {
        const DATA_MAX: usize = FIELD_DATA_MAX - FIELD_OVERHEAD - 1;
        let fill = |len: usize| {
            let mut buf = framed();
            let written =
                buf.write_nested(1, |inner| inner.write_field(1, &[0; FIELD_DATA_MAX][..len]));
            written.and_then(|()| buf.finish()).map(|()| buf.len())
        };
        // Заголовок кадра, заголовок внешней записи, вложенная запись и два терминатора
        // заполняют кадр ровно.
        assert_eq!(fill(DATA_MAX), Ok(MESSAGE_INLINE_MAX));
        assert_eq!(fill(DATA_MAX + 1), Err(IpcError::FrameOverflow));
    }

    #[test]
    fn finish_inside_a_nested_record_is_rejected() {
        let mut buf = MessageBuf::new();
        assert_eq!(
            buf.write_nested(1, MessageBuf::finish),
            Err(IpcError::FieldOrder)
        );
    }

    struct Probe;

    impl crate::Protocol for Probe {
        const ID: &'static str = "Probe";

        type Client<C: crate::Channel> = C;

        fn client<C: crate::Channel>(channel: C) -> C {
            channel
        }
    }

    fn port(raw: u32) -> Cap<kind::Port> {
        Cap::from_handle(Handle::from_abi(NonZeroU32::new(raw).expect("non-zero")))
    }

    #[test]
    fn a_session_field_encodes_as_a_port_capability() {
        let mut as_session = MessageBuf::new();
        crate::IntoWireValue::write_as_field(
            &Session::<Probe>::from_handle(port(0x2A)),
            &mut as_session,
            1,
        )
        .expect("session written");
        let mut as_cap = MessageBuf::new();
        crate::IntoWireValue::write_as_field(&port(0x2A), &mut as_cap, 1).expect("cap written");

        assert_eq!(as_session.as_bytes(), as_cap.as_bytes());
        assert_eq!(as_session.handles(), as_cap.handles());
        assert_eq!(
            <Session<Probe> as crate::WireTyped>::WIRE_TYPE,
            <Cap<kind::Port> as crate::WireTyped>::WIRE_TYPE
        );
    }

    #[test]
    fn a_session_arrives_owning_the_port_of_its_field() {
        let mut buf = MessageBuf::new();
        crate::IntoWireValue::write_as_field(
            &Session::<Probe>::from_handle(port(0x2B)),
            &mut buf,
            1,
        )
        .expect("session written");
        buf.finish().expect("finish ok");

        let field = first_field(buf.as_bytes()).expect("ok").expect("present");
        let session: Session<Probe, Cap<kind::Port>> =
            <Session<Probe> as crate::WireValue<crate::resources::testing::TestFrame>>::from_field(
                field.data,
                &mut crate::resources::testing::TestFrame::of(buf.handles()),
            )
            .expect("the session is read");
        assert_eq!(session.handle, port(0x2B));
    }

    #[test]
    fn handle_field_carries_the_slot_index() {
        let mut buf = MessageBuf::new();
        buf.write_handle(1, 0x2A).expect("first handle");
        buf.write_handle(2, 0x2B).expect("second handle");
        buf.finish().expect("finish ok");
        assert_eq!(buf.handles(), [0x2A, 0x2B]);
        assert_eq!(
            buf.as_bytes(),
            &[0x01, 0x01, 0x00, 0x00, 0x02, 0x01, 0x00, 0x01, 0x00]
        );
    }
}
