//! Вид узла дерева имён.

use ipc::{
    WireTyped, WireValue,
    schema::WireType,
    wire::{HandleSlots, IpcError, MessageBuf, value},
};

/// Вид узла: файл либо каталог.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Directory,
}

impl NodeKind {
    /// Дискриминант вида в кадре. Нумерация видов начинается с единицы.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        match self {
            Self::File => 1,
            Self::Directory => 2,
        }
    }

    /// Вид по дискриминанту. `None` - значение вне набора контракта.
    #[must_use]
    pub const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::File),
            2 => Some(Self::Directory),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_directory(self) -> bool {
        matches!(self, Self::Directory)
    }
}

impl<'a> WireValue<'a> for NodeKind {
    fn write_as_field<const N: usize>(
        &self,
        buf: &mut MessageBuf<N>,
        field_id: u8,
        _handles: &mut HandleSlots,
    ) -> Result<(), IpcError> {
        buf.write_field(field_id, &value::encode_u8(self.as_wire()))
    }

    fn from_field(data: &'a [u8], _handles: &HandleSlots) -> Result<Self, IpcError> {
        Self::from_wire(value::decode_u8(data)?).ok_or(IpcError::InvalidValue)
    }
}

impl WireTyped for NodeKind {
    const WIRE_TYPE: WireType = WireType::Uint(size_of::<u8>() as u8);
}

#[cfg(test)]
mod tests {
    use ipc::{
        WireValue,
        wire::{FieldCursor, HandleSlots, IpcError, MESSAGE_INLINE_MAX, MessageBuf},
    };

    use super::NodeKind;

    const FIELD_ID: u8 = 1;

    const KINDS: [NodeKind; 2] = [NodeKind::File, NodeKind::Directory];

    fn round_trip(kind: NodeKind) -> Result<NodeKind, IpcError> {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        kind.write_as_field(&mut buf, FIELD_ID, &mut HandleSlots::new())
            .expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        NodeKind::from_field(field.data, &HandleSlots::new())
    }

    #[test]
    fn every_kind_round_trips() {
        for kind in KINDS {
            assert_eq!(round_trip(kind), Ok(kind));
            assert_eq!(NodeKind::from_wire(kind.as_wire()), Some(kind));
        }
    }

    #[test]
    fn wire_discriminants_are_distinct_and_non_zero() {
        assert_ne!(NodeKind::File.as_wire(), NodeKind::Directory.as_wire());
        assert_eq!(NodeKind::from_wire(0), None);
    }

    #[test]
    fn discriminant_outside_the_set_is_rejected() {
        assert_eq!(NodeKind::from_wire(3), None);
        assert_eq!(
            NodeKind::from_field(&[0], &HandleSlots::new()),
            Err(IpcError::InvalidValue)
        );
        assert_eq!(
            NodeKind::from_field(&[3], &HandleSlots::new()),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn wrong_field_length_is_rejected() {
        assert_eq!(
            NodeKind::from_field(&[1, 0], &HandleSlots::new()),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn only_the_directory_is_a_directory() {
        assert!(NodeKind::Directory.is_directory());
        assert!(!NodeKind::File.is_directory());
    }
}
