//! Режим доступа открываемого дескриптора.

use ipc::{
    IntoWireValue, ReceivedResources, WireTyped, WireValue,
    schema::WireType,
    wire::{IpcError, MessageBuf, value},
};

/// Режим доступа дескриптора, в кадре занимает `u8`. Обязательный параметр `open`: дескриптор
/// без права на чтение и запись невыразим.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// Только чтение узла.
    Read,
    /// Только запись узла.
    Write,
    /// Чтение и запись узла.
    ReadWrite,
}

impl AccessMode {
    /// Дискриминант режима в кадре. Нумерация начинается с единицы.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        match self {
            Self::Read => 1,
            Self::Write => 2,
            Self::ReadWrite => 3,
        }
    }

    /// Режим по дискриминанту. `None` - значение вне набора контракта.
    #[must_use]
    pub const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Read),
            2 => Some(Self::Write),
            3 => Some(Self::ReadWrite),
            _ => None,
        }
    }

    /// Допускает ли режим чтение.
    #[must_use]
    pub const fn readable(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    /// Допускает ли режим запись.
    #[must_use]
    pub const fn writable(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

impl<'a, R: ReceivedResources> WireValue<'a, R> for AccessMode {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        Self::from_wire(value::decode_u8(data)?).ok_or(IpcError::InvalidValue)
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for AccessMode {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, &value::encode_u8(self.as_wire()))
    }
}

impl WireTyped for AccessMode {
    #[allow(clippy::cast_possible_truncation)]
    const WIRE_TYPE: WireType = WireType::Uint(size_of::<u8>() as u8);
}

#[cfg(test)]
mod tests {
    use ipc::{
        IntoWireValue, WireValue,
        wire::{FieldCursor, IpcError, MessageBuf, value},
    };

    use super::AccessMode;

    const FIELD_ID: u8 = 1;

    const MODES: [AccessMode; 3] = [AccessMode::Read, AccessMode::Write, AccessMode::ReadWrite];

    fn round_trip(mode: AccessMode) -> Result<AccessMode, IpcError> {
        let mut buf = MessageBuf::<()>::new();
        mode.write_into(&mut buf, FIELD_ID).expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        AccessMode::from_field(field.data, &mut ())
    }

    #[test]
    fn every_mode_round_trips() {
        for mode in MODES {
            assert_eq!(round_trip(mode), Ok(mode));
        }
    }

    #[test]
    fn a_mode_without_read_or_write_is_unrepresentable() {
        assert_eq!(
            AccessMode::from_field(&value::encode_u8(0), &mut ()),
            Err(IpcError::InvalidValue)
        );
        assert_eq!(
            AccessMode::from_field(&value::encode_u8(4), &mut ()),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn mode_capabilities_match_their_variants() {
        assert!(AccessMode::Read.readable() && !AccessMode::Read.writable());
        assert!(!AccessMode::Write.readable() && AccessMode::Write.writable());
        assert!(AccessMode::ReadWrite.readable() && AccessMode::ReadWrite.writable());
    }
}
