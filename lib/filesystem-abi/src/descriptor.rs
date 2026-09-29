//! Дескриптор узла, открытого в сессии клиента.

use core::num::NonZeroU16;

use ipc::{
    IntoWireValue, ReceivedResources, WireTyped, WireValue,
    schema::WireType,
    wire::{IpcError, MessageBuf, value},
};

/// Дескриптор открытого узла, в кадре занимает `u32` LE. Действителен только в своей сессии и
/// для клиента непрозрачен.
///
/// Сервер укладывает в значение слот и его поколение. Закрытый дескриптор не переназначается
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

impl<'a, R: ReceivedResources> WireValue<'a, R> for NodeDescriptor {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        Ok(Self(value::decode_u32(data)?))
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for NodeDescriptor {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, &value::encode_u32(self.0))
    }
}

impl WireTyped for NodeDescriptor {
    #[allow(clippy::cast_possible_truncation)]
    const WIRE_TYPE: WireType = WireType::Uint(size_of::<u32>() as u8);
}

#[cfg(test)]
mod tests {
    use ipc::{
        IntoWireValue, WireValue,
        wire::{FieldCursor, IpcError, MessageBuf},
    };

    use super::NodeDescriptor;

    const FIELD_ID: u8 = 1;

    fn round_trip(fd: NodeDescriptor) -> Result<NodeDescriptor, IpcError> {
        let mut buf = MessageBuf::<()>::new();
        fd.write_into(&mut buf, FIELD_ID).expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        NodeDescriptor::from_field(field.data, &mut ())
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
            NodeDescriptor::from_field(&[1, 0], &mut ()),
            Err(IpcError::InvalidValue)
        );
    }
}
