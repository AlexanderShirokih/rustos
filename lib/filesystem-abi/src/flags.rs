//! Флаги открытия узла: действия над деревом имён. Режим доступа дескриптора несёт отдельный
//! обязательный параметр `open` ([`crate::AccessMode`]).

use core::ops::BitOr;

use ipc::{
    WireTyped, WireValue,
    schema::WireType,
    wire::{HandleSlots, IpcError, MessageBuf, value},
};

/// Набор флагов открытия, в кадре занимает `u32` LE. Биты вне набора кадр переносит как есть.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags(u32);

impl OpenFlags {
    /// Пустой набор: открыть существующий узел без действий над деревом.
    pub const NONE: Self = Self(0);

    /// Создать узел, если его нет.
    pub const CREATE: Self = Self(1 << 0);

    /// Вместе с [`CREATE`](Self::CREATE) требует, чтобы узла не было.
    pub const EXCLUSIVE: Self = Self(1 << 1);

    /// Усечь содержимое узла при открытии.
    pub const TRUNCATE: Self = Self(1 << 2);

    /// Требует, чтобы узел был каталогом.
    pub const DIRECTORY: Self = Self(1 << 3);

    /// Содержит ли набор все флаги `other`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for OpenFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl<'a> WireValue<'a> for OpenFlags {
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

impl WireTyped for OpenFlags {
    #[allow(clippy::cast_possible_truncation)]
    const WIRE_TYPE: WireType = WireType::Uint(size_of::<u32>() as u8);
}

#[cfg(test)]
mod tests {
    use ipc::{
        WireValue,
        wire::{FieldCursor, HandleSlots, IpcError, MESSAGE_INLINE_MAX, MessageBuf, value},
    };

    use super::OpenFlags;

    const FIELD_ID: u8 = 1;

    const ALL_FLAGS: [OpenFlags; 4] = [
        OpenFlags::CREATE,
        OpenFlags::EXCLUSIVE,
        OpenFlags::TRUNCATE,
        OpenFlags::DIRECTORY,
    ];

    fn round_trip(flags: OpenFlags) -> Result<OpenFlags, IpcError> {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        flags
            .write_as_field(&mut buf, FIELD_ID, &mut HandleSlots::new())
            .expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        OpenFlags::from_field(field.data, &HandleSlots::new())
    }

    fn union() -> OpenFlags {
        ALL_FLAGS
            .into_iter()
            .fold(OpenFlags::NONE, |acc, flag| acc | flag)
    }

    #[test]
    fn flags_occupy_distinct_bits() {
        for (index, flag) in ALL_FLAGS.into_iter().enumerate() {
            for other in ALL_FLAGS.into_iter().skip(index + 1) {
                assert!(!flag.contains(other));
            }
        }
    }

    #[test]
    fn contains_holds_for_every_member_of_the_union() {
        for flag in ALL_FLAGS {
            assert!(union().contains(flag));
        }
        assert!(!(OpenFlags::CREATE | OpenFlags::TRUNCATE).contains(OpenFlags::DIRECTORY));
        assert!(OpenFlags::NONE.contains(OpenFlags::NONE));
    }

    #[test]
    fn combination_round_trips_through_a_field() {
        let flags = OpenFlags::CREATE | OpenFlags::EXCLUSIVE | OpenFlags::TRUNCATE;
        assert_eq!(round_trip(flags), Ok(flags));
    }

    #[test]
    fn bits_outside_the_set_survive_the_wire() {
        let flags = OpenFlags::from_field(&value::encode_u32(1 << 31), &HandleSlots::new())
            .expect("decode ok");
        assert!(!flags.contains(OpenFlags::CREATE));
        assert_eq!(round_trip(flags), Ok(flags));
    }

    #[test]
    fn wrong_field_length_is_rejected() {
        assert_eq!(
            OpenFlags::from_field(&[1, 0], &HandleSlots::new()),
            Err(IpcError::InvalidValue)
        );
    }
}
