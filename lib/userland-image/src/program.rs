use userland::{
    EntryView, Segment, SegmentPermissions, USERLAND_PAGE_SIZE, UserlandValidationError,
    validate_entry,
};

pub const USERLAND_PROGRAM_MAGIC: [u8; 8] = *b"USRPROG\0";
pub(crate) const PROGRAM_HEADER_SIZE: usize = 32;
pub(crate) const SEGMENT_SIZE: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProgramHeader {
    magic: [u8; 8],
    entry_va: u64,
    stack_size: u64,
    segment_count: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WireSegment {
    file_offset: u64,
    file_size: u64,
    va_base: u64,
    mem_size: u64,
    flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramDecodeError {
    BufferTooShort { needed: usize, actual: usize },
    InvalidMagic([u8; 8]),
    InvalidLayout(&'static str),
    OutOfBounds { offset: u64, size: u64 },
    Semantic(UserlandValidationError),
}

/// Программа, разобранная из содержимого записи образа. Путь приходит от
/// записи. Сам блоб пути не содержит.
#[derive(Debug, Clone, Copy)]
pub struct DecodedProgram<'a> {
    path: &'a str,
    data: &'a [u8],
    header: ProgramHeader,
}

struct DecodedSegments<'a> {
    data: &'a [u8],
    next_offset: usize,
    remaining: usize,
}

impl<'a> EntryView<'a> for DecodedProgram<'a> {
    type Segment = Segment<'a>;

    fn path(&self) -> &'a str {
        self.path
    }

    fn entry_va(&self) -> u64 {
        self.header.entry_va
    }

    fn stack_size(&self) -> u64 {
        self.header.stack_size
    }

    fn segments(&self) -> impl Iterator<Item = Self::Segment> + '_ {
        DecodedSegments {
            data: self.data,
            next_offset: PROGRAM_HEADER_SIZE,
            remaining: usize::from(self.header.segment_count),
        }
    }
}

impl<'a> Iterator for DecodedSegments<'a> {
    type Item = Segment<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        // Образ читается из чужой памяти, и неполная запись обрывает перечисление вместо
        // паники.
        let wire = decode_segment(self.data, self.next_offset)?;
        self.next_offset += SEGMENT_SIZE;
        self.remaining -= 1;

        let start = usize::try_from(wire.file_offset).ok()?;
        let size = usize::try_from(wire.file_size).ok()?;
        Some(Segment {
            va_base: wire.va_base,
            mem_size: wire.mem_size,
            permissions: decode_permissions(wire.flags)?,
            bytes: self.data.get(start..start + size)?,
        })
    }
}

/// Разбирает программу из байт `data`, называя её путём `path`. Длины сегментов проверяются
/// по фактическому срезу, и байты за концом программы разбору не мешают.
pub fn decode_program<'a>(
    path: &'a str,
    data: &'a [u8],
) -> Result<DecodedProgram<'a>, ProgramDecodeError> {
    let header = decode_program_header(data).ok_or(ProgramDecodeError::BufferTooShort {
        needed: PROGRAM_HEADER_SIZE,
        actual: data.len(),
    })?;
    if header.magic != USERLAND_PROGRAM_MAGIC {
        return Err(ProgramDecodeError::InvalidMagic(header.magic));
    }

    let descriptors_size = usize::from(header.segment_count)
        .checked_mul(SEGMENT_SIZE)
        .ok_or(ProgramDecodeError::InvalidLayout("segments_size_overflow"))?;
    let descriptors_end = descriptors_size
        .checked_add(PROGRAM_HEADER_SIZE)
        .ok_or(ProgramDecodeError::InvalidLayout("segments_size_overflow"))?;
    // Заголовок и дескрипторы занимают первую страницу записи, данные сегментов
    // начинаются за ней.
    if u64::try_from(descriptors_end).is_ok_and(|end| end > USERLAND_PAGE_SIZE) {
        return Err(ProgramDecodeError::InvalidLayout("descriptors_exceed_page"));
    }
    if descriptors_end > data.len() {
        return Err(ProgramDecodeError::BufferTooShort {
            needed: descriptors_end,
            actual: data.len(),
        });
    }

    validate_wire_segments(data, header)?;

    let program = DecodedProgram { path, data, header };
    validate_entry(&program).map_err(ProgramDecodeError::Semantic)?;
    Ok(program)
}

fn validate_wire_segments(data: &[u8], header: ProgramHeader) -> Result<(), ProgramDecodeError> {
    let data_len = u64::try_from(data.len())
        .map_err(|_| ProgramDecodeError::InvalidLayout("record_size_overflow"))?;
    // Данные сегментов лежат за первой страницей записи. Выравнивание va_base
    // проверяет validate_entry.
    let mut previous_end = USERLAND_PAGE_SIZE;

    for index in 0..usize::from(header.segment_count) {
        let offset = PROGRAM_HEADER_SIZE + index * SEGMENT_SIZE;
        let segment = decode_segment(data, offset)
            .ok_or(ProgramDecodeError::InvalidLayout("segment_bounds"))?;

        if decode_permissions(segment.flags).is_none() {
            return Err(ProgramDecodeError::InvalidLayout("segment_flags"));
        }
        if !segment.file_offset.is_multiple_of(USERLAND_PAGE_SIZE) {
            return Err(ProgramDecodeError::InvalidLayout("segment_alignment"));
        }
        let segment_end = segment.file_offset.checked_add(segment.file_size).ok_or(
            ProgramDecodeError::OutOfBounds {
                offset: segment.file_offset,
                size: segment.file_size,
            },
        )?;
        if segment.file_offset < previous_end || segment_end > data_len {
            return Err(ProgramDecodeError::OutOfBounds {
                offset: segment.file_offset,
                size: segment.file_size,
            });
        }
        previous_end = segment_end;
    }
    Ok(())
}

fn decode_program_header(bytes: &[u8]) -> Option<ProgramHeader> {
    Some(ProgramHeader {
        magic: bytes.get(0..8)?.try_into().ok()?,
        entry_va: u64::from_le_bytes(bytes.get(8..16)?.try_into().ok()?),
        stack_size: u64::from_le_bytes(bytes.get(16..24)?.try_into().ok()?),
        segment_count: u16::from_le_bytes(bytes.get(24..26)?.try_into().ok()?),
    })
}

fn decode_segment(bytes: &[u8], offset: usize) -> Option<WireSegment> {
    let bytes = bytes.get(offset..offset.checked_add(SEGMENT_SIZE)?)?;
    Some(WireSegment {
        file_offset: u64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?),
        file_size: u64::from_le_bytes(bytes.get(8..16)?.try_into().ok()?),
        va_base: u64::from_le_bytes(bytes.get(16..24)?.try_into().ok()?),
        mem_size: u64::from_le_bytes(bytes.get(24..32)?.try_into().ok()?),
        flags: u32::from_le_bytes(bytes.get(32..36)?.try_into().ok()?),
    })
}

fn decode_permissions(value: u32) -> Option<SegmentPermissions> {
    match value {
        0 => Some(SegmentPermissions::ReadWrite),
        1 => Some(SegmentPermissions::ReadOnly),
        2 => Some(SegmentPermissions::ReadExecute),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use userland::{Entry, Segment};

    use super::*;
    use crate::encode_program;

    const PATH: &str = "/boot/rootkeeper";

    fn segment(
        va_base: u64,
        permissions: SegmentPermissions,
        bytes: &'static [u8],
    ) -> Segment<'static> {
        Segment {
            va_base,
            mem_size: 0x1000,
            permissions,
            bytes,
        }
    }

    fn program_bytes() -> Vec<u8> {
        let segments = [
            segment(0x4000_0000, SegmentPermissions::ReadExecute, b"CODE"),
            segment(0x4000_1000, SegmentPermissions::ReadWrite, b"RW"),
        ];
        encode_program(&Entry {
            path: PATH,
            entry_va: 0x4000_0000,
            stack_size: 0x4000,
            segments: &segments,
        })
        .expect("encode program")
    }

    #[test]
    fn round_trip_places_segment_bytes_behind_the_first_page() {
        let bytes = program_bytes();
        let program = decode_program(PATH, &bytes).expect("decode program");
        let segments: Vec<_> = program.segments().collect();

        assert_eq!(program.path(), PATH);
        assert_eq!(program.entry_va(), 0x4000_0000);
        assert_eq!(segments[0].bytes, b"CODE");
        assert_eq!(segments[1].bytes, b"RW");

        let first = decode_segment(&bytes, PROGRAM_HEADER_SIZE).expect("first descriptor");
        let second =
            decode_segment(&bytes, PROGRAM_HEADER_SIZE + SEGMENT_SIZE).expect("second descriptor");
        assert_eq!(first.file_offset, USERLAND_PAGE_SIZE);
        assert_eq!(second.file_offset, 2 * USERLAND_PAGE_SIZE);
    }

    #[test]
    fn decode_reads_a_program_from_bytes_with_page_slack_past_its_end() {
        let mut bytes = program_bytes();
        let length = bytes.len();
        bytes.resize(length + USERLAND_PAGE_SIZE as usize, 0);

        let program =
            decode_program(PATH, &bytes).expect("slack past the program is not a segment");
        let segments: Vec<_> = program.segments().collect();
        assert_eq!(segments[0].bytes, b"CODE");
        assert_eq!(segments[1].bytes, b"RW");
    }

    #[test]
    fn rejects_foreign_magic() {
        assert_eq!(
            decode_program(PATH, b"plain file bytes").expect_err("short record is not a program"),
            ProgramDecodeError::BufferTooShort {
                needed: PROGRAM_HEADER_SIZE,
                actual: 16,
            }
        );

        let mut bytes = program_bytes();
        bytes[0] = b'X';
        assert!(matches!(
            decode_program(PATH, &bytes),
            Err(ProgramDecodeError::InvalidMagic(_))
        ));
    }

    #[test]
    fn rejects_segment_reaching_past_the_record() {
        let mut bytes = program_bytes();
        let file_size = PROGRAM_HEADER_SIZE + 8;
        bytes[file_size..file_size + 8].copy_from_slice(&u64::MAX.to_le_bytes());

        assert!(matches!(
            decode_program(PATH, &bytes),
            Err(ProgramDecodeError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn rejects_unaligned_segment_offset() {
        let mut bytes = program_bytes();
        bytes[PROGRAM_HEADER_SIZE..PROGRAM_HEADER_SIZE + 8]
            .copy_from_slice(&(USERLAND_PAGE_SIZE + 1).to_le_bytes());

        assert_eq!(
            decode_program(PATH, &bytes).expect_err("unaligned segment must fail"),
            ProgramDecodeError::InvalidLayout("segment_alignment")
        );
    }

    #[test]
    fn rejects_descriptors_beyond_the_first_page() {
        let mut bytes = program_bytes();
        bytes[24..26].copy_from_slice(&1024u16.to_le_bytes());

        assert_eq!(
            decode_program(PATH, &bytes).expect_err("descriptor block must fit a page"),
            ProgramDecodeError::InvalidLayout("descriptors_exceed_page")
        );
    }

    #[test]
    fn rejects_semantically_overlapping_segments() {
        let mut bytes = program_bytes();
        let second_va = PROGRAM_HEADER_SIZE + SEGMENT_SIZE + 16;
        bytes[second_va..second_va + 8].copy_from_slice(&0x4000_0000u64.to_le_bytes());

        assert_eq!(
            decode_program(PATH, &bytes).expect_err("overlap must fail"),
            ProgramDecodeError::Semantic(UserlandValidationError::OverlappingSegments)
        );
    }

    #[test]
    fn encoder_rejects_descriptors_beyond_the_first_page() {
        let count = (USERLAND_PAGE_SIZE as usize - PROGRAM_HEADER_SIZE) / SEGMENT_SIZE + 1;
        let segments: Vec<Segment<'_>> = (0..count)
            .map(|index| Segment {
                va_base: 0x4000_0000 + index as u64 * USERLAND_PAGE_SIZE,
                mem_size: 0x1000,
                permissions: SegmentPermissions::ReadExecute,
                bytes: b"CODE",
            })
            .collect();

        assert_eq!(
            encode_program(&Entry {
                path: PATH,
                entry_va: 0x4000_0000,
                stack_size: 0x4000,
                segments: &segments,
            })
            .expect_err("descriptor block must fit a page"),
            crate::ImageEncodeError::TooManySegments
        );
    }
}
