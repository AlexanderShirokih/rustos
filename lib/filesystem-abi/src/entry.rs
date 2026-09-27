//! Раскладка записи каталога в буфере данных сессии: `kind` (u8), `name_len` (u16 LE), байты
//! имени. Записи идут подряд без выравнивания.

use ipc::wire::{IpcError, value};

use crate::{kind::NodeKind, limits::NAME_MAX, path::Component};

/// Длина заголовка записи каталога.
pub const ENTRY_HEADER_SIZE: usize = size_of::<u8>() + size_of::<u16>();

/// Запись каталога в буфере данных.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry<'a> {
    /// Вид узла записи.
    pub kind: NodeKind,
    /// Имя записи: один компонент пути по правилам [`Component::new`].
    pub name: &'a str,
}

/// Разобранная запись каталога вместе с числом занятых ею байт.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedEntry<'a> {
    /// Запись из начала данных.
    pub entry: DirEntry<'a>,
    /// Число байт, занятых записью.
    pub len: usize,
}

impl<'a> DirEntry<'a> {
    const fn encoded_len(&self) -> usize {
        ENTRY_HEADER_SIZE + self.name.len()
    }

    /// Пишет запись в начало `out`, возвращая число занятых байт. `InvalidValue` - имя не
    /// является одним компонентом пути, `Truncated` - буфер короче записи.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, IpcError> {
        Component::new(self.name).map_err(|_| IpcError::InvalidValue)?;
        let len = self.encoded_len();
        let Some(record) = out.get_mut(..len) else {
            return Err(IpcError::Truncated);
        };
        let Some((kind, rest)) = record.split_first_mut() else {
            return Err(IpcError::Truncated);
        };
        let Some((name_len, name)) = rest.split_first_chunk_mut::<2>() else {
            return Err(IpcError::Truncated);
        };
        let encoded_name_len =
            u16::try_from(self.name.len()).map_err(|_| IpcError::InvalidValue)?;
        *kind = self.kind.as_wire();
        *name_len = value::encode_u16(encoded_name_len);
        name.copy_from_slice(self.name.as_bytes());
        Ok(len)
    }

    /// Читает запись из начала `data`. `Truncated` - данных меньше записи, `InvalidValue` - вид
    /// вне набора, имя не UTF-8 либо не один компонент пути.
    pub fn decode(data: &'a [u8]) -> Result<DecodedEntry<'a>, IpcError> {
        let Some(header) = data.get(..ENTRY_HEADER_SIZE) else {
            return Err(IpcError::Truncated);
        };
        let Some((kind, name_len)) = header.split_first() else {
            return Err(IpcError::Truncated);
        };
        let kind = NodeKind::from_wire(*kind).ok_or(IpcError::InvalidValue)?;
        let name_len = usize::from(value::decode_u16(name_len)?);
        if name_len > NAME_MAX {
            return Err(IpcError::InvalidValue);
        }
        let len = ENTRY_HEADER_SIZE + name_len;
        let Some(name) = data.get(ENTRY_HEADER_SIZE..len) else {
            return Err(IpcError::Truncated);
        };
        let name = value::decode_str(name)?;
        Component::new(name).map_err(|_| IpcError::InvalidValue)?;
        let entry = Self { kind, name };
        Ok(DecodedEntry { entry, len })
    }
}

#[cfg(test)]
mod tests {
    use ipc::wire::{IpcError, value};

    use super::{DirEntry, ENTRY_HEADER_SIZE};
    use crate::{kind::NodeKind, limits::NAME_MAX};

    const KIND: NodeKind = NodeKind::Directory;

    fn entry(name: &str) -> DirEntry<'_> {
        DirEntry { kind: KIND, name }
    }

    #[test]
    fn record_round_trips() {
        const NAME: &str = "sample";

        let mut buf = [0u8; 64];
        let written = entry(NAME).encode(&mut buf).expect("encode ok");
        assert_eq!(written, ENTRY_HEADER_SIZE + NAME.len());
        let decoded = DirEntry::decode(&buf).expect("decode ok");
        assert_eq!(decoded.len, written);
        assert_eq!(decoded.entry, entry(NAME));
    }

    #[test]
    fn record_layout_is_kind_then_length_then_name() {
        let mut buf = [0u8; 8];
        entry("ab").encode(&mut buf).expect("encode ok");
        assert_eq!(
            &buf[..ENTRY_HEADER_SIZE + 2],
            &[KIND.as_wire(), 2, 0, b'a', b'b']
        );
    }

    #[test]
    fn records_follow_each_other_without_padding() {
        let mut buf = [0u8; 32];
        let first = entry("a").encode(&mut buf).expect("first ok");
        let second = entry("bc").encode(&mut buf[first..]).expect("second ok");
        assert_eq!(first + second, 2 * ENTRY_HEADER_SIZE + 3);

        let head = DirEntry::decode(&buf).expect("decode first");
        assert_eq!(head.entry, entry("a"));
        let tail = DirEntry::decode(&buf[head.len..]).expect("decode second");
        assert_eq!(tail.entry, entry("bc"));
    }

    #[test]
    fn name_at_bound_round_trips() {
        let name = "n".repeat(NAME_MAX);
        let mut buf = [0u8; ENTRY_HEADER_SIZE + NAME_MAX];
        let written = entry(&name).encode(&mut buf).expect("encode ok");
        assert_eq!(written, buf.len());
        let decoded = DirEntry::decode(&buf).expect("decode ok");
        assert_eq!(decoded.entry.name, name);
    }

    #[test]
    fn encode_of_name_over_bound_is_rejected() {
        let name = "n".repeat(NAME_MAX + 1);
        let mut buf = [0u8; 128];
        assert_eq!(entry(&name).encode(&mut buf), Err(IpcError::InvalidValue));
    }

    #[test]
    fn decode_of_name_over_bound_is_rejected() {
        let mut wire = [0u8; 128];
        wire[0] = KIND.as_wire();
        wire[1..ENTRY_HEADER_SIZE].copy_from_slice(&value::encode_u16(NAME_MAX as u16 + 1));
        assert_eq!(DirEntry::decode(&wire), Err(IpcError::InvalidValue));
    }

    #[test]
    fn kind_outside_the_set_is_rejected() {
        let wire = [0, 1, 0, b'a'];
        assert_eq!(DirEntry::decode(&wire), Err(IpcError::InvalidValue));
        let wire = [7, 1, 0, b'a'];
        assert_eq!(DirEntry::decode(&wire), Err(IpcError::InvalidValue));
    }

    #[test]
    fn encode_into_short_buffer_is_truncated() {
        let mut buf = [0u8; ENTRY_HEADER_SIZE + 1];
        assert_eq!(entry("ab").encode(&mut buf), Err(IpcError::Truncated));
    }

    #[test]
    fn decode_of_short_buffer_is_truncated() {
        let mut buf = [0u8; 16];
        let written = entry("abc").encode(&mut buf).expect("encode ok");
        assert_eq!(
            DirEntry::decode(&buf[..ENTRY_HEADER_SIZE - 1]),
            Err(IpcError::Truncated)
        );
        assert_eq!(
            DirEntry::decode(&buf[..written - 1]),
            Err(IpcError::Truncated)
        );
    }

    #[test]
    fn non_utf8_name_is_rejected() {
        let wire = [KIND.as_wire(), 2, 0, 0xFF, 0xFE];
        assert_eq!(DirEntry::decode(&wire), Err(IpcError::InvalidValue));
    }

    #[test]
    fn name_other_than_one_component_is_rejected() {
        for name in ["", ".", "..", "/victim", "a/b", "tail/"] {
            let mut buf = [0u8; 16];
            assert_eq!(entry(name).encode(&mut buf), Err(IpcError::InvalidValue));

            let mut wire = [0u8; 16];
            wire[0] = KIND.as_wire();
            wire[1..ENTRY_HEADER_SIZE].copy_from_slice(&value::encode_u16(name.len() as u16));
            wire[ENTRY_HEADER_SIZE..ENTRY_HEADER_SIZE + name.len()]
                .copy_from_slice(name.as_bytes());
            assert_eq!(DirEntry::decode(&wire), Err(IpcError::InvalidValue));
        }
    }

    #[test]
    fn dotted_name_that_is_not_a_dot_entry_round_trips() {
        for name in ["...", ".hidden", "a.b"] {
            let mut buf = [0u8; 16];
            entry(name).encode(&mut buf).expect("encode ok");
            assert_eq!(DirEntry::decode(&buf).expect("decode ok").entry.name, name);
        }
    }
}
