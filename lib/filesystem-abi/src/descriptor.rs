//! Дескриптор узла, открытого в сессии клиента.

use core::num::NonZeroU16;

use ipc::{
    WireTyped, WireValue,
    schema::WireType,
    wire::{HandleSlots, IpcError, MessageBuf, value},
};

/// Дескриптор открытого узла, в кадре занимает `u32` LE. Действителен только в своей сессии и
/// для клиента непрозрачен.
///
/// Сервер укладывает в значение слот и его поколение: закрытый дескриптор не переназначается
/// другому узлу, и отставший вызов по нему получает `BadDescriptor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeDescriptor(u32);

impl NodeDescriptor {
    const SLOT_BITS: u32 = 16;
    const SLOT_MASK: u32 = (1 << Self::SLOT_BITS) - 1;

    /// Оборачивает сырой номер дескриптора.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Сырой номер дескриптора.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Собирает дескриптор из слота и его поколения.
    #[must_use]
    pub const fn pack(generation: NonZeroU16, slot: u32) -> Self {
        Self(((generation.get() as u32) << Self::SLOT_BITS) | (slot & Self::SLOT_MASK))
    }

    /// Номер слота сессии.
    #[must_use]
    pub const fn slot(self) -> u32 {
        self.0 & Self::SLOT_MASK
    }

    /// Поколение слота на момент выдачи. Ноль не выдаётся.
    #[must_use]
    pub const fn generation(self) -> u16 {
        (self.0 >> Self::SLOT_BITS) as u16
    }
}

impl<'a> WireValue<'a> for NodeDescriptor {
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

impl WireTyped for NodeDescriptor {
    #[allow(clippy::cast_possible_truncation)]
    const WIRE_TYPE: WireType = WireType::Uint(size_of::<u32>() as u8);
}

#[cfg(test)]
mod tests {
    use ipc::{
        WireValue,
        wire::{FieldCursor, HandleSlots, IpcError, MESSAGE_INLINE_MAX, MessageBuf},
    };

    use super::NodeDescriptor;

    const FIELD_ID: u8 = 1;

    fn round_trip(fd: NodeDescriptor) -> Result<NodeDescriptor, IpcError> {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        fd.write_as_field(&mut buf, FIELD_ID, &mut HandleSlots::new())
            .expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        NodeDescriptor::from_field(field.data, &HandleSlots::new())
    }

    #[test]
    fn number_round_trips_through_a_field() {
        for fd in [
            NodeDescriptor::from_raw(0),
            NodeDescriptor::from_raw(u32::MAX),
        ] {
            assert_eq!(round_trip(fd), Ok(fd));
        }
    }

    #[test]
    fn wrong_field_length_is_rejected() {
        assert_eq!(
            NodeDescriptor::from_field(&[1, 0], &HandleSlots::new()),
            Err(IpcError::InvalidValue)
        );
    }
}
