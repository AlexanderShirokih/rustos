//! Позиция перечисления каталога: сервер выдаёт её, клиент возвращает следующим вызовом.

use ipc::{
    WireTyped, WireValue,
    schema::WireType,
    wire::{HandleSlots, IpcError, MessageBuf, value},
};

/// Позиция перечисления каталога, в кадре занимает `u32` LE. Сервер выдаёт позицию в
/// [`Listed::next`](crate::Listed::next), клиент возвращает её следующим вызовом `list` того же
/// дескриптора. Устройство значения задаёт бекенд: позиция, выданная перечислением одного
/// каталога, на другом определяет корректно сформированный, но не оговорённый контрактом срез.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirCursor(u32);

impl DirCursor {
    /// Начало перечисления: первый слот каталога.
    pub const START: Self = Self(0);

    /// Оборачивает сырой номер слота.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Сырой номер слота.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl<'a> WireValue<'a> for DirCursor {
    fn write_as_field<const N: usize>(
        &self,
        buf: &mut MessageBuf<N>,
        field_id: u8,
        _handles: &mut HandleSlots,
    ) -> Result<(), IpcError> {
        buf.write_field(field_id, &value::encode_u32(self.0))
    }

    fn from_field(data: &'a [u8], _handles: &HandleSlots) -> Result<Self, IpcError> {
        Ok(Self(value::decode_u32(data)?))
    }
}

impl WireTyped for DirCursor {
    #[allow(clippy::cast_possible_truncation)]
    const WIRE_TYPE: WireType = WireType::Uint(size_of::<u32>() as u8);
}

#[cfg(test)]
mod tests {
    use ipc::{
        WireValue,
        wire::{FieldCursor, HandleSlots, IpcError, MESSAGE_INLINE_MAX, MessageBuf},
    };

    use super::DirCursor;

    const FIELD_ID: u8 = 1;

    fn round_trip(position: DirCursor) -> Result<DirCursor, IpcError> {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        position
            .write_as_field(&mut buf, FIELD_ID, &mut HandleSlots::new())
            .expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        DirCursor::from_field(field.data, &HandleSlots::new())
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
            DirCursor::from_field(&[1, 0], &HandleSlots::new()),
            Err(IpcError::InvalidValue)
        );
    }
}
