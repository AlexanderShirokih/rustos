//! Запись дерева имён: упорядоченный список монтирований, который владелец политики
//! загрузки читает из userland-образа при своём запуске.

use alloc::vec::Vec;

/// Путь записи образа с деревом имён загрузки.
pub const BOOT_MOUNT_TABLE_PATH: &str = "/boot/init/mounts";

pub const MOUNT_TABLE_MAGIC: [u8; 8] = *b"NSMOUNT\0";
pub const MOUNT_TABLE_VERSION: u16 = 1;

const TABLE_HEADER_SIZE: usize = 16;
const MOUNT_HEADER_SIZE: usize = 4;

/// Монтирование: префикс дерева имён и имя экземпляра его файловой системы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mount<'a> {
    pub path: &'a str,
    pub instance: &'a str,
}

/// Дерево имён, разобранное из записи образа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountTable<'a> {
    bytes: &'a [u8],
    count: u16,
}

/// Обход монтирований в порядке поднятия.
pub struct Mounts<'a> {
    table: MountTable<'a>,
    next_index: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountTableDecodeError {
    InvalidMagic([u8; 8]),
    InvalidVersion(u16),
    Truncated { needed: usize, actual: usize },
    InvalidLayout(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountTableEncodeError {
    EmptyString,
    StringTooLong,
    TooManyMounts,
}

/// Декодирует дерево имён. Буфер разбирается целиком: хвост сверх таблицы
/// отвергается.
pub fn decode_mount_table(bytes: &[u8]) -> Result<MountTable<'_>, MountTableDecodeError> {
    let header = bytes
        .get(..TABLE_HEADER_SIZE)
        .ok_or(MountTableDecodeError::Truncated {
            needed: TABLE_HEADER_SIZE,
            actual: bytes.len(),
        })?;
    let magic = *header
        .first_chunk::<8>()
        .ok_or_else(|| truncated(bytes, TABLE_HEADER_SIZE))?;
    if magic != MOUNT_TABLE_MAGIC {
        return Err(MountTableDecodeError::InvalidMagic(magic));
    }
    let version = read_u16(header, 8)?;
    if version != MOUNT_TABLE_VERSION {
        return Err(MountTableDecodeError::InvalidVersion(version));
    }
    let count = read_u16(header, 10)?;
    if read_u32(header, 12)? != 0 {
        return Err(MountTableDecodeError::InvalidLayout("reserved"));
    }

    let mut cursor = TABLE_HEADER_SIZE;
    for _ in 0..count {
        let (_, next) = parse_mount(bytes, cursor)?;
        cursor = next;
    }
    // Запись выдана окном своей страницы: хвост за таблицей - нули
    // дополнения, а всё прочее там - порча.
    let trailing = bytes
        .get(cursor..)
        .ok_or(MountTableDecodeError::InvalidLayout("trailing_bytes"))?;
    if trailing.iter().any(|byte| *byte != 0) {
        return Err(MountTableDecodeError::InvalidLayout("trailing_bytes"));
    }
    Ok(MountTable {
        bytes: bytes
            .get(TABLE_HEADER_SIZE..cursor)
            .ok_or(MountTableDecodeError::InvalidLayout("records"))?,
        count,
    })
}

/// Кодирует дерево имён в запись образа.
pub fn encode_mount_table(mounts: &[Mount<'_>]) -> Result<Vec<u8>, MountTableEncodeError> {
    let count = u16::try_from(mounts.len()).map_err(|_| MountTableEncodeError::TooManyMounts)?;
    let mut bytes = Vec::with_capacity(TABLE_HEADER_SIZE);
    bytes.extend_from_slice(&MOUNT_TABLE_MAGIC);
    bytes.extend_from_slice(&MOUNT_TABLE_VERSION.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());

    for mount in mounts {
        let path_len = string_len(mount.path)?;
        let instance_len = string_len(mount.instance)?;
        bytes.extend_from_slice(&path_len.to_le_bytes());
        bytes.extend_from_slice(&instance_len.to_le_bytes());
        bytes.extend_from_slice(mount.path.as_bytes());
        bytes.extend_from_slice(mount.instance.as_bytes());
    }
    Ok(bytes)
}

impl<'a> MountTable<'a> {
    /// Число монтирований дерева.
    #[must_use]
    pub fn len(&self) -> usize {
        usize::from(self.count)
    }

    /// Дерево без монтирований не накрывает ни одного пути.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Монтирование с номером `index` в порядке поднятия.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<Mount<'a>> {
        let mut cursor = 0;
        for position in 0..self.len() {
            let (mount, next) = parse_mount(self.bytes, cursor).ok()?;
            if position == index {
                return Some(mount);
            }
            cursor = next;
        }
        None
    }

    /// Обходит монтирования в порядке поднятия.
    #[must_use]
    pub const fn iter(&self) -> Mounts<'a> {
        Mounts {
            table: *self,
            next_index: 0,
        }
    }
}

impl<'a> IntoIterator for MountTable<'a> {
    type IntoIter = Mounts<'a>;
    type Item = Mount<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &MountTable<'a> {
    type IntoIter = Mounts<'a>;
    type Item = Mount<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> Iterator for Mounts<'a> {
    type Item = Mount<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let mount = self.table.get(usize::from(self.next_index))?;
        self.next_index += 1;
        Some(mount)
    }
}

fn parse_mount(bytes: &[u8], at: usize) -> Result<(Mount<'_>, usize), MountTableDecodeError> {
    let header = bytes
        .get(at..at + MOUNT_HEADER_SIZE)
        .ok_or(truncated(bytes, at + MOUNT_HEADER_SIZE))?;
    let path_len = usize::from(read_u16(header, 0)?);
    let instance_len = usize::from(read_u16(header, 2)?);
    let (path, cursor) = parse_str(bytes, at + MOUNT_HEADER_SIZE, path_len, "mount_path")?;
    let (instance, cursor) = parse_str(bytes, cursor, instance_len, "mount_instance")?;
    Ok((Mount { path, instance }, cursor))
}

fn parse_str<'a>(
    bytes: &'a [u8],
    at: usize,
    len: usize,
    field: &'static str,
) -> Result<(&'a str, usize), MountTableDecodeError> {
    if len == 0 {
        return Err(MountTableDecodeError::InvalidLayout(field));
    }
    let end = at + len;
    let text = bytes.get(at..end).ok_or(truncated(bytes, end))?;
    let text =
        core::str::from_utf8(text).map_err(|_| MountTableDecodeError::InvalidLayout(field))?;
    Ok((text, end))
}

fn read_u16(bytes: &[u8], at: usize) -> Result<u16, MountTableDecodeError> {
    let field = bytes
        .get(at..at + 2)
        .ok_or(truncated(bytes, at + 2))?
        .try_into()
        .map_err(|_| truncated(bytes, at + 2))?;
    Ok(u16::from_le_bytes(field))
}

fn read_u32(bytes: &[u8], at: usize) -> Result<u32, MountTableDecodeError> {
    let field = bytes
        .get(at..at + 4)
        .ok_or(truncated(bytes, at + 4))?
        .try_into()
        .map_err(|_| truncated(bytes, at + 4))?;
    Ok(u32::from_le_bytes(field))
}

fn string_len(text: &str) -> Result<u16, MountTableEncodeError> {
    if text.is_empty() {
        return Err(MountTableEncodeError::EmptyString);
    }
    u16::try_from(text.len()).map_err(|_| MountTableEncodeError::StringTooLong)
}

fn truncated(bytes: &[u8], needed: usize) -> MountTableDecodeError {
    MountTableDecodeError::Truncated {
        needed,
        actual: bytes.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTS: &[Mount<'static>] = &[
        Mount {
            path: "/",
            instance: "filesystem-imagefs",
        },
        Mount {
            path: "/tmp",
            instance: "filesystem-ramfs",
        },
    ];

    fn table_bytes() -> Vec<u8> {
        encode_mount_table(MOUNTS).expect("encode mount table")
    }

    #[test]
    fn round_trip_keeps_the_order_of_mounts() {
        let bytes = table_bytes();
        let table = decode_mount_table(&bytes).expect("decode mount table");

        assert_eq!(table.len(), 2);
        assert_eq!(table.get(0), Some(MOUNTS[0]));
        assert_eq!(table.get(1), Some(MOUNTS[1]));
        assert_eq!(table.get(2), None);
        assert_eq!(table.iter().collect::<Vec<_>>(), MOUNTS);
    }

    #[test]
    fn round_trip_keeps_an_empty_tree() {
        let bytes = encode_mount_table(&[]).expect("encode mount table");
        let table = decode_mount_table(&bytes).expect("decode mount table");

        assert!(table.is_empty());
        assert_eq!(table.iter().next(), None);
    }

    #[test]
    fn decode_rejects_foreign_magic() {
        let mut bytes = table_bytes();
        bytes[0] = b'X';
        assert!(matches!(
            decode_mount_table(&bytes),
            Err(MountTableDecodeError::InvalidMagic(_))
        ));
    }

    #[test]
    fn decode_rejects_another_version() {
        let mut bytes = table_bytes();
        bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(
            decode_mount_table(&bytes).expect_err("version must match"),
            MountTableDecodeError::InvalidVersion(2)
        );
    }

    #[test]
    fn decode_rejects_a_truncated_record() {
        let bytes = table_bytes();
        for length in [0, TABLE_HEADER_SIZE, bytes.len() - 1] {
            assert!(matches!(
                decode_mount_table(&bytes[..length]),
                Err(MountTableDecodeError::Truncated { .. })
            ));
        }
    }

    #[test]
    fn decode_rejects_bytes_past_the_last_mount() {
        let mut bytes = table_bytes();
        bytes.push(1);
        assert_eq!(
            decode_mount_table(&bytes).expect_err("trailing bytes must fail"),
            MountTableDecodeError::InvalidLayout("trailing_bytes")
        );
    }

    #[test]
    fn decode_reads_a_table_padded_with_zeroes() {
        // Запись выдана окном страницы: за таблицей идут нули дополнения.
        let bytes = table_bytes();
        let mut padded = bytes.clone();
        padded.resize(bytes.len() + 64, 0);

        assert_eq!(
            decode_mount_table(&padded).expect("padding decodes"),
            decode_mount_table(&bytes).expect("table decodes")
        );
    }

    #[test]
    fn decode_rejects_a_mount_without_an_instance() {
        let mut bytes = table_bytes();
        bytes[TABLE_HEADER_SIZE + 2..TABLE_HEADER_SIZE + 4].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            decode_mount_table(&bytes).expect_err("mount names its instance"),
            MountTableDecodeError::InvalidLayout("mount_instance")
        );
    }

    #[test]
    fn encode_rejects_a_mount_without_a_path() {
        let nameless = [Mount {
            path: "",
            instance: "filesystem-ramfs",
        }];
        assert_eq!(
            encode_mount_table(&nameless).expect_err("path must be non-empty"),
            MountTableEncodeError::EmptyString
        );
    }
}
