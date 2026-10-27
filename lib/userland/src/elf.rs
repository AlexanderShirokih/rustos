//! Чтение ELF64 для AArch64 в порядке байт little-endian. Разбор принимает только
//! исполняемый файл `ET_EXEC` и отвергает остальные виды ELF.
//!
//! `ElfFile` проверяет заголовок файла и границы таблицы программных заголовков, затем
//! отдаёт точку входа, программные заголовки и диапазон байт секции по имени. Права
//! сегментов, выравнивание их смещений и смысл секций определяет вызывающий.

use core::ops::Range;

pub(crate) const ELF_HEADER_SIZE: usize = 64;
pub(crate) const PROGRAM_HEADER_SIZE: usize = 56;
pub(crate) const SECTION_HEADER_SIZE: usize = 64;

pub(crate) const ELF_MAGIC: [u8; 4] = *b"\x7fELF";
pub(crate) const ELFCLASS64: u8 = 2;
pub(crate) const ELFDATA2LSB: u8 = 1;
pub(crate) const EV_CURRENT: u32 = 1;
pub(crate) const ET_EXEC: u16 = 2;
pub(crate) const EM_AARCH64: u16 = 183;

pub(crate) const PT_LOAD: u32 = 1;
pub(crate) const PT_GNU_STACK: u32 = 0x6474_e551;
pub(crate) const PF_X: u32 = 1;
pub(crate) const PF_W: u32 = 2;
pub(crate) const PF_R: u32 = 4;

const SHT_NOBITS: u32 = 8;

pub(crate) const ERROR_HEADER: &str = "elf_header";
pub(crate) const ERROR_PROGRAM_HEADERS_OVERFLOW: &str = "program_headers_overflow";
pub(crate) const ERROR_PROGRAM_HEADER_SIZE: &str = "program_header_size";
pub(crate) const ERROR_SECTION_HEADER_SIZE: &str = "section_header_size";
pub(crate) const ERROR_SECTION_HEADERS_OVERFLOW: &str = "section_headers_overflow";
pub(crate) const ERROR_SECTION_HEADERS: &str = "section_headers";
pub(crate) const ERROR_SECTION_NAMES: &str = "section_names";
pub(crate) const ERROR_SECTION_WITHOUT_BYTES: &str = "section_without_bytes";
pub(crate) const ERROR_MAGIC: &str = "elf_magic";
pub(crate) const ERROR_CLASS: &str = "elf_class";
pub(crate) const ERROR_BYTE_ORDER: &str = "elf_data";
pub(crate) const ERROR_VERSION: &str = "elf_version";
pub(crate) const ERROR_TYPE: &str = "elf_type";
pub(crate) const ERROR_MACHINE: &str = "elf_machine";

pub(crate) const FILE_CLASS: usize = 4;
pub(crate) const FILE_BYTE_ORDER: usize = 5;
pub(crate) const FILE_TYPE: usize = 16;
pub(crate) const FILE_MACHINE: usize = 18;
pub(crate) const FILE_VERSION: usize = 20;
pub(crate) const FILE_ENTRY: usize = 24;
pub(crate) const FILE_PROGRAM_TABLE: usize = 32;
pub(crate) const FILE_SECTION_TABLE: usize = 40;
pub(crate) const FILE_PROGRAM_HEADER_SIZE: usize = 54;
pub(crate) const FILE_PROGRAM_COUNT: usize = 56;
pub(crate) const FILE_SECTION_HEADER_SIZE: usize = 58;
pub(crate) const FILE_SECTION_COUNT: usize = 60;
pub(crate) const FILE_SECTION_NAMES_INDEX: usize = 62;

pub(crate) const PROGRAM_KIND: usize = 0;
pub(crate) const PROGRAM_FLAGS: usize = 4;
pub(crate) const PROGRAM_OFFSET: usize = 8;
pub(crate) const PROGRAM_VA_BASE: usize = 16;
pub(crate) const PROGRAM_FILE_SIZE: usize = 32;
pub(crate) const PROGRAM_MEM_SIZE: usize = 40;

pub(crate) const SECTION_NAME: usize = 0;
pub(crate) const SECTION_KIND: usize = 4;
pub(crate) const SECTION_OFFSET: usize = 24;
pub(crate) const SECTION_SIZE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ElfError {
    TooShort { needed: usize, actual: usize },
    Layout(&'static str),
    OutOfBounds { offset: u64, size: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProgramHeader {
    pub(crate) kind: u32,
    pub(crate) flags: u32,
    pub(crate) offset: u64,
    pub(crate) va_base: u64,
    pub(crate) file_size: u64,
    pub(crate) mem_size: u64,
}

impl ProgramHeader {
    fn read(bytes: &[u8]) -> Option<Self> {
        Some(Self {
            kind: u32_at(bytes, PROGRAM_KIND)?,
            flags: u32_at(bytes, PROGRAM_FLAGS)?,
            offset: u64_at(bytes, PROGRAM_OFFSET)?,
            va_base: u64_at(bytes, PROGRAM_VA_BASE)?,
            file_size: u64_at(bytes, PROGRAM_FILE_SIZE)?,
            mem_size: u64_at(bytes, PROGRAM_MEM_SIZE)?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ElfFile<'a> {
    data: &'a [u8],
    programs: &'a [u8],
}

impl<'a> ElfFile<'a> {
    pub(crate) fn parse(data: &'a [u8]) -> Result<Self, ElfError> {
        let header = data.get(..ELF_HEADER_SIZE).ok_or(ElfError::TooShort {
            needed: ELF_HEADER_SIZE,
            actual: data.len(),
        })?;
        check_identity(header)?;

        let program_count =
            usize::from(u16_at(header, FILE_PROGRAM_COUNT).ok_or(ElfError::Layout(ERROR_HEADER))?);
        let program_table = u64_at(header, FILE_PROGRAM_TABLE)
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or(ElfError::Layout(ERROR_PROGRAM_HEADERS_OVERFLOW))?;

        let table_end = program_count
            .checked_mul(PROGRAM_HEADER_SIZE)
            .and_then(|size| size.checked_add(program_table))
            .ok_or(ElfError::Layout(ERROR_PROGRAM_HEADERS_OVERFLOW))?;
        let programs = data
            .get(program_table..table_end)
            .ok_or(ElfError::TooShort {
                needed: table_end,
                actual: data.len(),
            })?;

        Ok(Self { data, programs })
    }

    pub(crate) fn len(&self) -> usize {
        self.data.len()
    }

    pub(crate) fn entry(&self) -> u64 {
        u64_at(self.data, FILE_ENTRY).unwrap_or(0)
    }

    pub(crate) fn program_headers(&self) -> impl Iterator<Item = ProgramHeader> + use<'a> {
        self.programs
            .as_chunks::<PROGRAM_HEADER_SIZE>()
            .0
            .iter()
            .map_while(|bytes| ProgramHeader::read(bytes))
    }

    pub(crate) fn bytes(&self, offset: u64, size: u64) -> Option<&'a [u8]> {
        // У пустого диапазона смещение ничего не задаёт.
        if size == 0 {
            return Some(&[]);
        }
        let start = usize::try_from(offset).ok()?;

        self.data
            .get(start..start.checked_add(usize::try_from(size).ok()?)?)
    }

    pub(crate) fn slice(&self, range: Range<usize>) -> Option<&'a [u8]> {
        self.data.get(range)
    }

    pub(crate) fn section_range(&self, name: &str) -> Result<Option<Range<usize>>, ElfError> {
        let data = self.data;

        let count =
            usize::from(u16_at(data, FILE_SECTION_COUNT).ok_or(ElfError::Layout(ERROR_HEADER))?);
        let names_index = usize::from(
            u16_at(data, FILE_SECTION_NAMES_INDEX).ok_or(ElfError::Layout(ERROR_HEADER))?,
        );
        if usize::from(u16_at(data, FILE_SECTION_HEADER_SIZE).unwrap_or(0)) != SECTION_HEADER_SIZE {
            return Err(ElfError::Layout(ERROR_SECTION_HEADER_SIZE));
        }

        let table = u64_at(data, FILE_SECTION_TABLE)
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or(ElfError::Layout(ERROR_SECTION_HEADERS_OVERFLOW))?;
        let table_end = count
            .checked_mul(SECTION_HEADER_SIZE)
            .and_then(|size| size.checked_add(table))
            .ok_or(ElfError::Layout(ERROR_SECTION_HEADERS_OVERFLOW))?;
        if table_end > data.len() {
            return Err(ElfError::TooShort {
                needed: table_end,
                actual: data.len(),
            });
        }
        if names_index >= count {
            return Err(ElfError::Layout(ERROR_SECTION_NAMES));
        }

        // Имена секций лежат в секции с индексом из заголовка файла.
        let names = data
            .get(section_extent(data, table, names_index)?)
            .ok_or(ElfError::Layout(ERROR_SECTION_NAMES))?;

        for index in 0..count {
            let base = table + index * SECTION_HEADER_SIZE;
            let name_offset = data
                .get(base..)
                .and_then(|section| u32_at(section, SECTION_NAME))
                .and_then(|offset| usize::try_from(offset).ok())
                .ok_or(ElfError::Layout(ERROR_SECTION_HEADERS))?;

            // Имя в таблице строк оканчивается нулём: префикс без него совпал бы с более длинным
            // именем.
            let matches = names
                .get(name_offset..)
                .and_then(|candidate| candidate.strip_prefix(name.as_bytes()))
                .is_some_and(|rest| rest.first() == Some(&0));

            if matches {
                return section_extent(data, table, index).map(Some);
            }
        }

        Ok(None)
    }
}

fn check_identity(header: &[u8]) -> Result<(), ElfError> {
    if header.get(..ELF_MAGIC.len()) != Some(ELF_MAGIC.as_slice()) {
        return Err(ElfError::Layout(ERROR_MAGIC));
    }
    if header.get(FILE_CLASS) != Some(&ELFCLASS64) {
        return Err(ElfError::Layout(ERROR_CLASS));
    }
    if header.get(FILE_BYTE_ORDER) != Some(&ELFDATA2LSB) {
        return Err(ElfError::Layout(ERROR_BYTE_ORDER));
    }
    if u32_at(header, FILE_VERSION) != Some(EV_CURRENT) {
        return Err(ElfError::Layout(ERROR_VERSION));
    }
    if u16_at(header, FILE_TYPE) != Some(ET_EXEC) {
        return Err(ElfError::Layout(ERROR_TYPE));
    }
    if u16_at(header, FILE_MACHINE) != Some(EM_AARCH64) {
        return Err(ElfError::Layout(ERROR_MACHINE));
    }
    if usize::from(u16_at(header, FILE_PROGRAM_HEADER_SIZE).unwrap_or(0)) != PROGRAM_HEADER_SIZE {
        return Err(ElfError::Layout(ERROR_PROGRAM_HEADER_SIZE));
    }

    Ok(())
}

fn section_extent(data: &[u8], table: usize, index: usize) -> Result<Range<usize>, ElfError> {
    let section = index
        .checked_mul(SECTION_HEADER_SIZE)
        .and_then(|offset| offset.checked_add(table))
        .and_then(|offset| data.get(offset..))
        .ok_or(ElfError::Layout(ERROR_SECTION_HEADERS))?;
    let kind = u32_at(section, SECTION_KIND).ok_or(ElfError::Layout(ERROR_SECTION_HEADERS))?;
    let offset = u64_at(section, SECTION_OFFSET).ok_or(ElfError::Layout(ERROR_SECTION_HEADERS))?;
    let size = u64_at(section, SECTION_SIZE).ok_or(ElfError::Layout(ERROR_SECTION_HEADERS))?;
    // Секция без байтов в файле не может нести данные, которые читает вызывающий.
    if kind == SHT_NOBITS {
        return Err(ElfError::Layout(ERROR_SECTION_WITHOUT_BYTES));
    }

    usize::try_from(offset)
        .ok()
        .zip(usize::try_from(size).ok())
        .and_then(|(start, size)| Some(start..start.checked_add(size)?))
        .filter(|range| range.end <= data.len())
        .ok_or(ElfError::OutOfBounds { offset, size })
}

fn field<const N: usize>(bytes: &[u8], offset: usize) -> Option<[u8; N]> {
    bytes.get(offset..offset.checked_add(N)?)?.try_into().ok()
}

fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    field(bytes, offset).map(u16::from_le_bytes)
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    field(bytes, offset).map(u32::from_le_bytes)
}

fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    field(bytes, offset).map(u64::from_le_bytes)
}

#[cfg(test)]
pub(crate) fn section_header_at(bytes: &[u8], index: usize) -> usize {
    let table = usize::try_from(u64_at(bytes, FILE_SECTION_TABLE).expect("section table offset"))
        .expect("offset fits usize");
    table + index * SECTION_HEADER_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf_write::{Executable, LoadSegment, write_executable};

    const PAGE: u64 = 4096;
    const SECTION: &str = ".note.test";

    fn executable_bytes() -> Vec<u8> {
        let segments = [
            LoadSegment {
                flags: PF_R | PF_X,
                va_base: 0x4000_0000,
                mem_size: 0x1000,
                bytes: b"CODE",
            },
            LoadSegment {
                flags: PF_R | PF_W,
                va_base: 0x4000_1000,
                mem_size: 0x1000,
                bytes: b"RW",
            },
        ];
        write_executable(&Executable {
            entry: 0x4000_0000,
            stack_size: 0x4000,
            segments: &segments,
            segment_align: PAGE,
            section_name: SECTION,
            section: b"NOTE",
        })
        .expect("write executable")
    }

    fn put(bytes: &mut [u8], offset: usize, value: &[u8]) {
        bytes[offset..offset + value.len()].copy_from_slice(value);
    }

    #[test]
    fn round_trip_reads_headers_and_the_named_section() {
        let bytes = executable_bytes();
        let elf = ElfFile::parse(&bytes).expect("parse");

        assert_eq!(elf.entry(), 0x4000_0000);
        let headers: Vec<_> = elf.program_headers().collect();
        assert_eq!(
            headers.iter().map(|header| header.kind).collect::<Vec<_>>(),
            [PT_LOAD, PT_LOAD, PT_GNU_STACK]
        );
        assert_eq!(headers[0].offset, PAGE);
        assert_eq!(headers[1].offset, 2 * PAGE);
        assert_eq!(headers[2].mem_size, 0x4000);
        assert_eq!(elf.bytes(headers[0].offset, 4), Some(b"CODE".as_slice()));

        let range = elf
            .section_range(SECTION)
            .expect("section table")
            .expect("section");
        assert_eq!(elf.slice(range), Some(b"NOTE".as_slice()));
        assert_eq!(elf.section_range(".other").expect("section table"), None);
    }

    #[test]
    fn alignment_covers_headers_larger_than_a_page() {
        let segments: Vec<_> = (0..103u64)
            .map(|index| LoadSegment {
                flags: PF_R | PF_X,
                va_base: 0x4000_0000 + index * PAGE,
                mem_size: PAGE,
                bytes: b"CODE",
            })
            .collect();
        let bytes = write_executable(&Executable {
            entry: 0x4000_0000,
            stack_size: PAGE,
            segments: &segments,
            segment_align: PAGE,
            section_name: SECTION,
            section: &[0xAA; 4096],
        })
        .expect("headers span pages");

        let elf = ElfFile::parse(&bytes).expect("parse");
        let headers_end = (ELF_HEADER_SIZE + 104 * PROGRAM_HEADER_SIZE) as u64;
        for header in elf
            .program_headers()
            .filter(|header| header.kind == PT_LOAD)
        {
            assert_eq!(header.offset % PAGE, 0);
            assert!(header.offset >= headers_end);
        }
    }

    #[test]
    fn rejects_foreign_bytes_architecture_and_object_kind() {
        assert_eq!(
            ElfFile::parse(b"plain file bytes").expect_err("short bytes are not ELF"),
            ElfError::TooShort {
                needed: ELF_HEADER_SIZE,
                actual: 16,
            }
        );

        for (offset, value) in [
            (0, b"X".as_slice()),
            (FILE_TYPE, &3u16.to_le_bytes()),
            (FILE_MACHINE, &243u16.to_le_bytes()),
            (FILE_CLASS, &[1]),
            (FILE_BYTE_ORDER, &[2]),
        ] {
            let mut bytes = executable_bytes();
            put(&mut bytes, offset, value);
            assert!(
                matches!(ElfFile::parse(&bytes), Err(ElfError::Layout(_))),
                "patch at {offset}"
            );
        }
    }

    #[test]
    fn rejects_truncated_program_headers() {
        let mut bytes = executable_bytes();
        put(&mut bytes, FILE_PROGRAM_COUNT, &1024u16.to_le_bytes());

        assert_eq!(
            ElfFile::parse(&bytes).expect_err("header table must fit the file"),
            ElfError::TooShort {
                needed: ELF_HEADER_SIZE + 1024 * PROGRAM_HEADER_SIZE,
                actual: bytes.len()
            }
        );
    }

    #[test]
    fn section_range_rejects_extent_past_the_file() {
        for size in [u64::MAX, 1 << 40] {
            let mut bytes = executable_bytes();
            let section = section_header_at(&bytes, 1);
            put(&mut bytes, section + SECTION_SIZE, &size.to_le_bytes());

            let elf = ElfFile::parse(&bytes).expect("parse");
            assert!(
                matches!(
                    elf.section_range(SECTION),
                    Err(ElfError::OutOfBounds { .. })
                ),
                "section size {size}"
            );
        }
    }
}
