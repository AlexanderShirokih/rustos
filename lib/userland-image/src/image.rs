use userland::USERLAND_PAGE_SIZE;

use crate::program::{DecodedProgram, ProgramDecodeError, decode_program};

pub const USERLAND_IMAGE_MAGIC: [u8; 8] = *b"USRLIMG\0";
pub const USERLAND_IMAGE_VERSION: u16 = 1;
pub(crate) const IMAGE_HEADER_SIZE: usize = 24;
pub(crate) const RECORD_SIZE: usize = 28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImageHeader {
    magic: [u8; 8],
    version: u16,
    entry_count: u16,
    boot_index: u16,
    total_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecordHeader {
    path_offset: u64,
    path_len: u16,
    alias_of: u16,
    data_offset: u64,
    data_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageDecodeError {
    BufferTooShort {
        needed: usize,
        actual: usize,
    },
    InvalidMagic([u8; 8]),
    InvalidVersion(u16),
    InvalidLayout {
        entry_index: Option<usize>,
        detail: &'static str,
    },
    OutOfBounds {
        entry_index: usize,
        offset: u64,
        size: u64,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct DecodedImage<'a> {
    bytes: &'a [u8],
    header: ImageHeader,
}

/// Запись таблицы: путь и байты содержимого. Что это за содержимое, знает тот,
/// кто его разбирает.
#[derive(Debug, Clone, Copy)]
pub struct DecodedEntry<'a> {
    path: &'a str,
    data: &'a [u8],
    data_offset: u64,
}

pub struct DecodedEntries<'a> {
    image: DecodedImage<'a>,
    next_index: usize,
}

/// Декодирует образ из буфера. Длина буфера должна быть не меньше размера, объявленного в
/// заголовке.
pub fn decode(bytes: &[u8]) -> Result<DecodedImage<'_>, ImageDecodeError> {
    DecodedImage::decode(bytes)
}

impl<'a> DecodedImage<'a> {
    fn decode(bytes: &'a [u8]) -> Result<Self, ImageDecodeError> {
        let header = decode_image_header(bytes).ok_or(ImageDecodeError::BufferTooShort {
            needed: IMAGE_HEADER_SIZE,
            actual: bytes.len(),
        })?;
        if header.magic != USERLAND_IMAGE_MAGIC {
            return Err(ImageDecodeError::InvalidMagic(header.magic));
        }
        if header.version != USERLAND_IMAGE_VERSION {
            return Err(ImageDecodeError::InvalidVersion(header.version));
        }
        if header.entry_count == 0 {
            return Err(invalid_layout(None, "entry_count"));
        }
        if header.boot_index >= header.entry_count {
            return Err(invalid_layout(None, "boot_index"));
        }

        let total_size =
            usize::try_from(header.total_size).map_err(|_| invalid_layout(None, "total_size"))?;
        // Буфер не короче образа; хвост сверх объявленного заголовком размера -
        // паддинг страницы при выдаче региона, декодируем ровно префикс.
        if total_size > bytes.len() {
            return Err(ImageDecodeError::BufferTooShort {
                needed: total_size,
                actual: bytes.len(),
            });
        }
        let bytes = &bytes[..total_size];

        let table_end = table_end(header.entry_count).ok_or(invalid_layout(None, "table_size"))?;
        if table_end > total_size {
            return Err(invalid_layout(None, "table_bounds"));
        }

        let image = Self { bytes, header };
        let mut previous_path: Option<&str> = None;
        let mut data_end: u64 = 0;
        for index in 0..image.entry_count() {
            let entry = parse_entry(bytes, header.entry_count, index)?;
            // Записи отсортированы строго по возрастанию пути: на этом стоит
            // двоичный поиск, а строгость запрещает дубликаты путей.
            if previous_path.is_some_and(|previous| previous >= entry.path) {
                return Err(invalid_layout(Some(index), "path_order"));
            }
            previous_path = Some(entry.path);

            // Собственные окна лежат в порядке таблицы и не пересекаются, псевдоним
            // повторяет окно более ранней записи целиком.
            let record = decode_record_header(bytes, IMAGE_HEADER_SIZE + index * RECORD_SIZE)
                .ok_or(invalid_layout(Some(index), "record_bounds"))?;
            match record.alias_of {
                0 => {
                    if record.data_offset < data_end {
                        return Err(invalid_layout(Some(index), "data_overlap"));
                    }
                    data_end = record.data_offset.saturating_add(record.data_size);
                }
                alias => {
                    let aliased = usize::from(alias - 1);
                    if aliased >= index {
                        return Err(invalid_layout(Some(index), "alias_order"));
                    }
                    let owner =
                        decode_record_header(bytes, IMAGE_HEADER_SIZE + aliased * RECORD_SIZE)
                            .ok_or(invalid_layout(Some(index), "record_bounds"))?;
                    if owner.alias_of != 0
                        || owner.data_offset != record.data_offset
                        || owner.data_size != record.data_size
                    {
                        return Err(invalid_layout(Some(index), "alias_window"));
                    }
                }
            }
        }
        Ok(image)
    }

    pub fn entry_count(&self) -> usize {
        usize::from(self.header.entry_count)
    }

    /// Запись, с которой поднимается bootstrap-процесс.
    pub fn boot_entry(&self) -> DecodedEntry<'a> {
        self.entry_at(usize::from(self.header.boot_index))
    }

    /// Возвращает запись с заданным путём двоичным поиском по таблице.
    pub fn entry(&self, path: &str) -> Option<DecodedEntry<'a>> {
        let (mut low, mut high) = (0, self.entry_count());
        while low < high {
            let middle = low + (high - low) / 2;
            let entry = self.entry_at(middle);
            match entry.path.cmp(path) {
                core::cmp::Ordering::Less => low = middle + 1,
                core::cmp::Ordering::Greater => high = middle,
                core::cmp::Ordering::Equal => return Some(entry),
            }
        }
        None
    }

    pub fn entries(&self) -> DecodedEntries<'a> {
        DecodedEntries {
            image: *self,
            next_index: 0,
        }
    }

    fn entry_at(&self, index: usize) -> DecodedEntry<'a> {
        parse_entry(self.bytes, self.header.entry_count, index)
            .expect("entries were validated during decode")
    }
}

impl<'a> DecodedEntry<'a> {
    pub fn path(&self) -> &'a str {
        self.path
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Смещение байт содержимого от начала образа, кратное [`USERLAND_PAGE_SIZE`].
    pub fn data_offset(&self) -> u64 {
        self.data_offset
    }

    /// Разбирает содержимое записи как программу.
    pub fn program(&self) -> Result<DecodedProgram<'a>, ProgramDecodeError> {
        decode_program(self.path, self.data)
    }
}

impl<'a> Iterator for DecodedEntries<'a> {
    type Item = DecodedEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_index == self.image.entry_count() {
            return None;
        }
        let entry = self.image.entry_at(self.next_index);
        self.next_index += 1;
        Some(entry)
    }
}

fn parse_entry(
    bytes: &[u8],
    entry_count: u16,
    index: usize,
) -> Result<DecodedEntry<'_>, ImageDecodeError> {
    let table_end = table_end(entry_count).ok_or(invalid_layout(None, "table_size"))?;
    let header = decode_record_header(bytes, IMAGE_HEADER_SIZE + index * RECORD_SIZE).ok_or(
        ImageDecodeError::BufferTooShort {
            needed: table_end,
            actual: bytes.len(),
        },
    )?;

    let path_offset = usize::try_from(header.path_offset)
        .map_err(|_| invalid_layout(Some(index), "path_bounds"))?;
    let path_end = path_offset
        .checked_add(usize::from(header.path_len))
        .ok_or(invalid_layout(Some(index), "path_bounds"))?;
    if path_offset < table_end {
        return Err(invalid_layout(Some(index), "path_overlaps_table"));
    }
    let path_bytes = bytes
        .get(path_offset..path_end)
        .ok_or(invalid_layout(Some(index), "path_bounds"))?;
    let path =
        core::str::from_utf8(path_bytes).map_err(|_| invalid_layout(Some(index), "path_utf8"))?;

    if !header.data_offset.is_multiple_of(USERLAND_PAGE_SIZE) {
        return Err(invalid_layout(Some(index), "data_alignment"));
    }
    let data_offset =
        usize::try_from(header.data_offset).map_err(|_| out_of_bounds(index, header))?;
    let data_size = usize::try_from(header.data_size).map_err(|_| out_of_bounds(index, header))?;
    let data_end = data_offset
        .checked_add(data_size)
        .ok_or(out_of_bounds(index, header))?;
    if data_offset < table_end {
        return Err(invalid_layout(Some(index), "data_overlaps_table"));
    }
    let data = bytes
        .get(data_offset..data_end)
        .ok_or(out_of_bounds(index, header))?;

    Ok(DecodedEntry {
        path,
        data,
        data_offset: header.data_offset,
    })
}

fn table_end(entry_count: u16) -> Option<usize> {
    usize::from(entry_count)
        .checked_mul(RECORD_SIZE)?
        .checked_add(IMAGE_HEADER_SIZE)
}

fn decode_image_header(bytes: &[u8]) -> Option<ImageHeader> {
    Some(ImageHeader {
        magic: bytes.get(0..8)?.try_into().ok()?,
        version: u16::from_le_bytes(bytes.get(8..10)?.try_into().ok()?),
        entry_count: u16::from_le_bytes(bytes.get(10..12)?.try_into().ok()?),
        boot_index: u16::from_le_bytes(bytes.get(12..14)?.try_into().ok()?),
        total_size: u64::from_le_bytes(bytes.get(16..24)?.try_into().ok()?),
    })
}

fn decode_record_header(bytes: &[u8], offset: usize) -> Option<RecordHeader> {
    let bytes = bytes.get(offset..offset.checked_add(RECORD_SIZE)?)?;
    Some(RecordHeader {
        path_offset: u64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?),
        path_len: u16::from_le_bytes(bytes.get(8..10)?.try_into().ok()?),
        alias_of: u16::from_le_bytes(bytes.get(10..12)?.try_into().ok()?),
        data_offset: u64::from_le_bytes(bytes.get(12..20)?.try_into().ok()?),
        data_size: u64::from_le_bytes(bytes.get(20..28)?.try_into().ok()?),
    })
}

fn invalid_layout(entry_index: Option<usize>, detail: &'static str) -> ImageDecodeError {
    ImageDecodeError::InvalidLayout {
        entry_index,
        detail,
    }
}

fn out_of_bounds(entry_index: usize, header: RecordHeader) -> ImageDecodeError {
    ImageDecodeError::OutOfBounds {
        entry_index,
        offset: header.data_offset,
        size: header.data_size,
    }
}

#[cfg(test)]
mod tests {
    use userland::{Entry, EntryView, Segment, SegmentPermissions};

    use super::*;
    use crate::{ImageEncodeError, ImageRecord, encode, encode_program};

    const BOOT_PATH: &str = "/boot/rootkeeper";

    fn program_blob() -> Vec<u8> {
        let segments = [
            Segment {
                va_base: 0x4000_0000,
                mem_size: 0x1000,
                permissions: SegmentPermissions::ReadExecute,
                bytes: b"CODE",
            },
            Segment {
                va_base: 0x4000_1000,
                mem_size: 0x1000,
                permissions: SegmentPermissions::ReadWrite,
                bytes: b"RW",
            },
        ];
        encode_program(&Entry {
            path: BOOT_PATH,
            entry_va: 0x4000_0000,
            stack_size: 0x4000,
            segments: &segments,
        })
        .expect("encode program")
    }

    fn image_bytes() -> Vec<u8> {
        let program = program_blob();
        encode(
            &[
                ImageRecord {
                    path: "/data/sample",
                    data: b"hello",
                },
                ImageRecord {
                    path: BOOT_PATH,
                    data: &program,
                },
            ],
            BOOT_PATH,
        )
        .expect("encode image")
    }

    fn record_field(bytes: &[u8], index: usize, offset: usize) -> u64 {
        let at = IMAGE_HEADER_SIZE + index * RECORD_SIZE + offset;
        u64::from_le_bytes(bytes[at..at + 8].try_into().expect("field is eight bytes"))
    }

    fn record_alias(bytes: &[u8], index: usize) -> u16 {
        let at = IMAGE_HEADER_SIZE + index * RECORD_SIZE + 10;
        u16::from_le_bytes(bytes[at..at + 2].try_into().expect("field is two bytes"))
    }

    fn patch(bytes: &mut [u8], at: usize, value: u64) {
        bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn patch_alias(bytes: &mut [u8], index: usize, value: u16) {
        let at = IMAGE_HEADER_SIZE + index * RECORD_SIZE + 10;
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn alias_record(bytes: &mut [u8], index: usize, owner: usize) {
        let offset = record_field(bytes, owner, 12);
        let size = record_field(bytes, owner, 20);
        patch(bytes, IMAGE_HEADER_SIZE + index * RECORD_SIZE + 12, offset);
        patch(bytes, IMAGE_HEADER_SIZE + index * RECORD_SIZE + 20, size);
        patch_alias(
            bytes,
            index,
            u16::try_from(owner + 1).expect("record index"),
        );
    }

    #[test]
    fn round_trip_exposes_records_and_program() {
        let bytes = image_bytes();
        let image = decode(&bytes).expect("decode");

        let boot = image.boot_entry();
        assert_eq!(boot.path(), BOOT_PATH);

        let program = boot.program().expect("boot record carries a program");
        let segments: Vec<_> = program.segments().collect();
        assert_eq!(program.entry_va(), 0x4000_0000);
        assert_eq!(program.stack_size(), 0x4000);
        assert_eq!(segments[0].bytes, b"CODE");
        assert_eq!(segments[1].permissions, SegmentPermissions::ReadWrite);

        let sample = image.entry("/data/sample").expect("file record");
        assert_eq!(sample.data(), b"hello");
    }

    #[test]
    fn entries_are_sorted_by_path_and_boot_index_follows() {
        let bytes = image_bytes();
        let image = decode(&bytes).expect("decode");

        let paths: Vec<_> = image.entries().map(|entry| entry.path()).collect();
        assert_eq!(paths, [BOOT_PATH, "/data/sample"]);
        assert_eq!(image.boot_entry().path(), BOOT_PATH);
    }

    #[test]
    fn record_data_is_page_aligned() {
        let bytes = image_bytes();
        let image = decode(&bytes).expect("decode");

        for index in 0..image.entry_count() {
            let data_offset = record_field(&bytes, index, 12);
            assert_eq!(data_offset % USERLAND_PAGE_SIZE, 0);
        }
    }

    #[test]
    fn entry_reports_the_offset_of_its_data() {
        let bytes = image_bytes();
        let image = decode(&bytes).expect("decode");

        for (index, entry) in image.entries().enumerate() {
            let offset = entry.data_offset();
            assert_eq!(offset, record_field(&bytes, index, 12));
            assert_eq!(
                &bytes[offset as usize..offset as usize + entry.data().len()],
                entry.data()
            );
        }
    }

    #[test]
    fn records_without_bytes_take_no_space() {
        let program = program_blob();
        let bytes = encode(
            &[
                ImageRecord {
                    path: "/boot/empty",
                    data: b"",
                },
                ImageRecord {
                    path: BOOT_PATH,
                    data: &program,
                },
                ImageRecord {
                    path: "/data/empty",
                    data: b"",
                },
            ],
            BOOT_PATH,
        )
        .expect("encode image");
        let image = decode(&bytes).expect("decode");

        // Пустая запись стоит на смещении следующей за ней, а вторая делит с ней
        // одно окно нулевой длины.
        let head = image.entry("/boot/empty").expect("file record");
        assert!(head.data().is_empty());
        assert_eq!(head.data_offset(), image.boot_entry().data_offset());

        let tail = image.entry("/data/empty").expect("file record");
        assert!(tail.data().is_empty());
        assert_eq!(tail.data_offset(), head.data_offset());
    }

    #[test]
    fn records_with_equal_content_share_one_window() {
        let program = program_blob();
        let bytes = encode(
            &[
                ImageRecord {
                    path: BOOT_PATH,
                    data: &program,
                },
                ImageRecord {
                    path: "/data/first",
                    data: b"same",
                },
                ImageRecord {
                    path: "/data/second",
                    data: b"same",
                },
            ],
            BOOT_PATH,
        )
        .expect("encode image");
        let image = decode(&bytes).expect("decode");

        let first = image.entry("/data/first").expect("file record");
        let second = image.entry("/data/second").expect("file record");
        assert_eq!(first.data(), b"same");
        assert_eq!(second.data(), b"same");
        assert_eq!(second.data_offset(), first.data_offset());
        // Повторившая содержимое запись ссылается на открывшую окно.
        assert_eq!(record_alias(&bytes, 1), 0);
        assert_eq!(record_alias(&bytes, 2), 2);
        // Повторившееся содержимое своей страницы образа не занимает.
        assert_eq!(bytes.len() as u64, first.data_offset() + 4);
    }

    #[test]
    fn lookup_by_path_is_exact() {
        let bytes = image_bytes();
        let image = decode(&bytes).expect("decode");

        assert_eq!(
            image.entry(BOOT_PATH).expect("boot record").path(),
            BOOT_PATH
        );
        assert!(image.entry("/data").is_none());
        assert!(image.entry("/data/sample/x").is_none());
    }

    #[test]
    fn encode_rejects_missing_boot_record() {
        let records = [ImageRecord {
            path: "/data/sample",
            data: b"hello",
        }];
        assert_eq!(
            encode(&records, BOOT_PATH).expect_err("boot path must be present"),
            ImageEncodeError::BootRecordMissing
        );
    }

    #[test]
    fn encode_rejects_duplicate_paths() {
        let program = program_blob();
        let records = [
            ImageRecord {
                path: BOOT_PATH,
                data: &program,
            },
            ImageRecord {
                path: BOOT_PATH,
                data: b"clash",
            },
        ];
        assert_eq!(
            encode(&records, BOOT_PATH).expect_err("duplicate paths must fail"),
            ImageEncodeError::DuplicatePath
        );
    }

    #[test]
    fn decode_tolerates_trailing_padding() {
        let mut bytes = image_bytes();
        bytes.extend(core::iter::repeat_n(0u8, 4096));

        let image = decode(&bytes).expect("padded buffer decodes");
        assert_eq!(image.boot_entry().path(), BOOT_PATH);
    }

    #[test]
    fn decode_rejects_buffer_shorter_than_total_size() {
        let bytes = image_bytes();
        assert!(matches!(
            decode(&bytes[..bytes.len() - 1]),
            Err(ImageDecodeError::BufferTooShort { .. })
        ));
    }

    #[test]
    fn decode_rejects_foreign_magic() {
        let mut bytes = image_bytes();
        bytes[0] = b'X';
        assert!(matches!(
            decode(&bytes),
            Err(ImageDecodeError::InvalidMagic(_))
        ));
    }

    #[test]
    fn decode_rejects_boot_index_outside_table() {
        let mut bytes = image_bytes();
        bytes[12..14].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(
            decode(&bytes).expect_err("boot_index must be in range"),
            invalid_layout(None, "boot_index")
        );
    }

    #[test]
    fn decode_rejects_unsorted_table() {
        let mut bytes = image_bytes();
        // Меняем местами описания путей у двух записей: таблица перестаёт возрастать.
        let second = IMAGE_HEADER_SIZE + RECORD_SIZE;
        let first_path: [u8; 12] = bytes[IMAGE_HEADER_SIZE..IMAGE_HEADER_SIZE + 12]
            .try_into()
            .expect("path fields of a record");
        let second_path: [u8; 12] = bytes[second..second + 12]
            .try_into()
            .expect("path fields of a record");
        bytes[IMAGE_HEADER_SIZE..IMAGE_HEADER_SIZE + 12].copy_from_slice(&second_path);
        bytes[second..second + 12].copy_from_slice(&first_path);

        assert_eq!(
            decode(&bytes).expect_err("descending paths must fail"),
            invalid_layout(Some(1), "path_order")
        );
    }

    #[test]
    fn decode_rejects_unaligned_data_offset() {
        let mut bytes = image_bytes();
        let data_offset = record_field(&bytes, 0, 12);
        patch(&mut bytes, IMAGE_HEADER_SIZE + 12, data_offset + 1);

        assert_eq!(
            decode(&bytes).expect_err("unaligned data must fail"),
            invalid_layout(Some(0), "data_alignment")
        );
    }

    #[test]
    fn decode_rejects_data_outside_image() {
        let mut bytes = image_bytes();
        patch(&mut bytes, IMAGE_HEADER_SIZE + 20, u64::MAX);

        assert!(matches!(
            decode(&bytes),
            Err(ImageDecodeError::OutOfBounds { entry_index: 0, .. })
        ));
    }

    #[test]
    fn decode_rejects_path_inside_table() {
        let mut bytes = image_bytes();
        patch(&mut bytes, IMAGE_HEADER_SIZE, 0);

        assert_eq!(
            decode(&bytes).expect_err("path inside table must fail"),
            invalid_layout(Some(0), "path_overlaps_table")
        );
    }

    #[test]
    fn decode_rejects_overlapping_data_windows() {
        let mut bytes = image_bytes();
        // Окно второй записи сажается на окно первой: границы образа и таблицы
        // при этом не нарушены.
        let first_data_offset = record_field(&bytes, 0, 12);
        patch(
            &mut bytes,
            IMAGE_HEADER_SIZE + RECORD_SIZE + 12,
            first_data_offset,
        );

        assert_eq!(
            decode(&bytes).expect_err("overlapping data windows must fail"),
            invalid_layout(Some(1), "data_overlap")
        );
    }

    #[test]
    fn decode_accepts_a_record_repeating_an_earlier_window() {
        let mut bytes = image_bytes();
        alias_record(&mut bytes, 1, 0);

        let image = decode(&bytes).expect("repeated data window decodes");
        assert_eq!(
            image.entry("/data/sample").expect("file record").data(),
            image.boot_entry().data()
        );
    }

    #[test]
    fn decode_rejects_an_alias_that_does_not_repeat_the_window() {
        // Окно псевдонима лежит внутри чужого, но повторяет его не целиком.
        let mut bytes = image_bytes();
        alias_record(&mut bytes, 1, 0);
        let size = record_field(&bytes, 1, 20);
        patch(&mut bytes, IMAGE_HEADER_SIZE + RECORD_SIZE + 20, size - 1);

        assert_eq!(
            decode(&bytes).expect_err("partial repeat must fail"),
            invalid_layout(Some(1), "alias_window")
        );
    }

    #[test]
    fn decode_rejects_an_alias_of_a_record_it_does_not_follow() {
        let mut bytes = image_bytes();
        alias_record(&mut bytes, 1, 0);
        // Запись ссылается на себя: окно повторено, но его владельца в таблице нет.
        patch_alias(&mut bytes, 1, 2);

        assert_eq!(
            decode(&bytes).expect_err("alias outside the earlier records must fail"),
            invalid_layout(Some(1), "alias_order")
        );
    }

    #[test]
    fn decode_rejects_an_alias_of_an_alias() {
        let program = program_blob();
        let mut bytes = encode(
            &[
                ImageRecord {
                    path: BOOT_PATH,
                    data: &program,
                },
                ImageRecord {
                    path: "/data/a",
                    data: b"same",
                },
                ImageRecord {
                    path: "/data/b",
                    data: b"same",
                },
                ImageRecord {
                    path: "/data/c",
                    data: b"same",
                },
            ],
            BOOT_PATH,
        )
        .expect("encode image");
        // Записи с повторившимся содержимым ссылаются на открывшую окно, а не друг на друга.
        assert_eq!(record_alias(&bytes, 2), 2);
        assert_eq!(record_alias(&bytes, 3), 2);

        // Владельцем окна объявляется сам псевдоним: цепочка ссылок образу не принадлежит.
        patch_alias(&mut bytes, 3, 3);
        assert_eq!(
            decode(&bytes).expect_err("alias of an alias must fail"),
            invalid_layout(Some(3), "alias_window")
        );
    }
}
