//! Позиция перечисления каталога. Сервер выдаёт её, клиент возвращает следующим вызовом.

use ipc::{
    IntoWireValue, ReceivedResources, WireTyped, WireValue,
    schema::WireType,
    wire::{IpcError, MessageBuf, value},
};

/// Позиция перечисления каталога, в кадре занимает `u32` LE. Сервер выдаёт позицию в
/// [`Listed::next`](crate::Listed::next), клиент возвращает её следующим вызовом `list` того же
/// дескриптора. Устройство значения задаёт бекенд. Позиция, выданная перечислением одного
/// каталога, на другом определяет корректно сформированный, но не оговорённый контрактом срез.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirCursor(u32);

impl DirCursor {
    /// Начало перечисления: первая запись каталога.
    pub const START: Self = Self(0);

    /// Оборачивает значение, пришедшее в кадре.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Значение позиции для укладки в кадр.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl<'a, R: ReceivedResources> WireValue<'a, R> for DirCursor {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        Ok(Self(value::decode_u32(data)?))
    }
}

impl IntoWireValue for DirCursor {
    type Read = Self;

    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
        buf.write_field(field_id, &value::encode_u32(self.0))
    }

    fn relinquish(self) {}
}

impl WireTyped for DirCursor {
    #[allow(clippy::cast_possible_truncation)]
    const WIRE_TYPE: WireType = WireType::Uint(size_of::<u32>() as u8);
}

#[cfg(test)]
mod tests {
    use ipc::{
        IntoWireValue, WireValue,
        wire::{FieldCursor, IpcError, MessageBuf},
    };

    use super::DirCursor;

    const FIELD_ID: u8 = 1;

    fn round_trip(position: DirCursor) -> Result<DirCursor, IpcError> {
        let mut buf = MessageBuf::new();
        position
            .write_as_field(&mut buf, FIELD_ID)
            .expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        DirCursor::from_field(field.data, &mut ())
    }

    #[test]
    fn position_round_trips_through_a_field() {
        for position in [DirCursor::START, DirCursor::from_raw(u32::MAX)] {
            assert_eq!(round_trip(position), Ok(position));
        }
    }

    #[test]
    fn wrong_field_length_is_rejected() {
        assert_eq!(
            DirCursor::from_field(&[1, 0], &mut ()),
            Err(IpcError::InvalidValue)
        );
    }
}
