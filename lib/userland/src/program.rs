#[cfg(any(test, feature = "alloc"))]
use alloc::vec::Vec;

#[cfg(any(test, feature = "alloc"))]
use crate::SegmentView;
use crate::{
    ProgramView, Segment, SegmentPermissions, USERLAND_PAGE_SIZE, UserlandValidationError,
    validate_program,
};

pub const USERLAND_PROGRAM_MAGIC: [u8; 8] = *b"USRPROG\0";
pub const USERLAND_PROGRAM_VERSION: u8 = 1;
pub(crate) const PROGRAM_HEADER_SIZE: usize = 31;
pub(crate) const SEGMENT_SIZE: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProgramHeader {
    magic: [u8; 8],
    entry_va: u64,
    stack_size: u64,
    segment_count: u16,
    version: u8,
    manifest_size: u32,
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
    /// Буфер не покрывает метаданные программы.
    BufferTooShort { needed: usize, actual: usize },
    /// Магия не соответствует формату программы.
    InvalidMagic([u8; 8]),
    /// Версия формата программы не поддерживается.
    InvalidVersion(u8),
    /// Раскладка заголовка или сегментов нарушает указанное правило.
    InvalidLayout(&'static str),
    /// Диапазон данных сегмента выходит за пределы программы.
    OutOfBounds { offset: u64, size: u64 },
    /// Модель программы нарушает условия загрузки.
    Semantic(UserlandValidationError),
}

#[cfg(any(test, feature = "alloc"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramEncodeError {
    /// Число сегментов не представимо в формате.
    TooManySegments,
    /// Размер или смещение не представимы в формате либо адресном пространстве.
    SizeOverflow,
    /// Модель программы нарушает условия загрузки.
    Semantic(UserlandValidationError),
}

/// Программа, заимствующая байты, из которых она разобрана.
#[derive(Debug, Clone, Copy)]
pub struct DecodedProgram<'a> {
    data: &'a [u8],
    header: ProgramHeader,
    manifest: &'a [u8],
}

struct DecodedSegments<'a> {
    data: &'a [u8],
    next_offset: usize,
    remaining: usize,
}

impl<'a> ProgramView<'a> for DecodedProgram<'a> {
    type Segment = Segment<'a>;

    fn manifest(&self) -> &'a [u8] {
        self.manifest
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
        // Программа читается из чужой памяти, и неполная запись обрывает перечисление вместо
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

pub fn decode_program(data: &[u8]) -> Result<DecodedProgram<'_>, ProgramDecodeError> {
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
    if header.version != USERLAND_PROGRAM_VERSION {
        return Err(ProgramDecodeError::InvalidVersion(header.version));
    }
    let manifest_size = usize::try_from(header.manifest_size)
        .map_err(|_| ProgramDecodeError::InvalidLayout("manifest_size_overflow"))?;
    let metadata_end = descriptors_end
        .checked_add(manifest_size)
        .ok_or(ProgramDecodeError::InvalidLayout("metadata_size_overflow"))?;
    let manifest =
        data.get(descriptors_end..metadata_end)
            .ok_or(ProgramDecodeError::BufferTooShort {
                needed: metadata_end,
                actual: data.len(),
            })?;
    let data_start = u64::try_from(metadata_end)
        .ok()
        .and_then(align_up_to_page)
        .ok_or(ProgramDecodeError::InvalidLayout("metadata_size_overflow"))?;
    validate_wire_segments(data, header, data_start)?;

    let program = DecodedProgram {
        data,
        header,
        manifest,
    };
    validate_program(&program).map_err(ProgramDecodeError::Semantic)?;
    Ok(program)
}

fn validate_wire_segments(
    data: &[u8],
    header: ProgramHeader,
    data_start: u64,
) -> Result<(), ProgramDecodeError> {
    let data_len = u64::try_from(data.len())
        .map_err(|_| ProgramDecodeError::InvalidLayout("program_size_overflow"))?;
    let mut previous_end = data_start;

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
        entry_va: u64::from_le_bytes(bytes.get(9..17)?.try_into().ok()?),
        stack_size: u64::from_le_bytes(bytes.get(17..25)?.try_into().ok()?),
        segment_count: u16::from_le_bytes(bytes.get(25..27)?.try_into().ok()?),
        version: *bytes.get(8)?,
        manifest_size: u32::from_le_bytes(bytes.get(27..31)?.try_into().ok()?),
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

#[cfg(any(test, feature = "alloc"))]
/// Кодирует программу в `USRPROG`. Данные сегментов выровнены по странице после метаданных.
pub fn encode_program<'a, E: ProgramView<'a>>(program: &E) -> Result<Vec<u8>, ProgramEncodeError> {
    validate_program(program).map_err(ProgramEncodeError::Semantic)?;
    let segment_count = u16::try_from(program.segments().count())
        .map_err(|_| ProgramEncodeError::TooManySegments)?;
    let descriptors_end = usize::from(segment_count)
        .checked_mul(SEGMENT_SIZE)
        .and_then(|size| size.checked_add(PROGRAM_HEADER_SIZE))
        .ok_or(ProgramEncodeError::SizeOverflow)?;
    let manifest_size =
        u32::try_from(program.manifest().len()).map_err(|_| ProgramEncodeError::SizeOverflow)?;
    let metadata_end = descriptors_end
        .checked_add(program.manifest().len())
        .ok_or(ProgramEncodeError::SizeOverflow)?;
    let mut cursor = u64::try_from(metadata_end).map_err(|_| ProgramEncodeError::SizeOverflow)?;
    let mut descriptors = Vec::with_capacity(usize::from(segment_count) * SEGMENT_SIZE);
    let mut segments = Vec::new();

    for segment in program.segments() {
        let file_offset = align_up_to_page(cursor).ok_or(ProgramEncodeError::SizeOverflow)?;
        let file_size =
            u64::try_from(segment.bytes().len()).map_err(|_| ProgramEncodeError::SizeOverflow)?;
        cursor = file_offset
            .checked_add(file_size)
            .ok_or(ProgramEncodeError::SizeOverflow)?;

        descriptors.extend_from_slice(&file_offset.to_le_bytes());
        descriptors.extend_from_slice(&file_size.to_le_bytes());
        descriptors.extend_from_slice(&segment.va_base().to_le_bytes());
        descriptors.extend_from_slice(&segment.mem_size().to_le_bytes());
        descriptors.extend_from_slice(&encode_permissions(segment.permissions()).to_le_bytes());
        descriptors.extend_from_slice(&0u32.to_le_bytes());
        segments.push((file_offset, segment.bytes()));
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&USERLAND_PROGRAM_MAGIC);
    bytes.push(USERLAND_PROGRAM_VERSION);
    bytes.extend_from_slice(&program.entry_va().to_le_bytes());
    bytes.extend_from_slice(&program.stack_size().to_le_bytes());
    bytes.extend_from_slice(&segment_count.to_le_bytes());
    bytes.extend_from_slice(&manifest_size.to_le_bytes());
    bytes.extend_from_slice(&descriptors);
    bytes.extend_from_slice(program.manifest());

    for (file_offset, segment_bytes) in segments {
        let file_offset =
            usize::try_from(file_offset).map_err(|_| ProgramEncodeError::SizeOverflow)?;
        bytes.resize(file_offset, 0);
        bytes.extend_from_slice(segment_bytes);
    }
    Ok(bytes)
}

fn align_up_to_page(offset: u64) -> Option<u64> {
    offset.checked_next_multiple_of(USERLAND_PAGE_SIZE)
}

#[cfg(any(test, feature = "alloc"))]
const fn encode_permissions(permissions: SegmentPermissions) -> u32 {
    match permissions {
        SegmentPermissions::ReadWrite => 0,
        SegmentPermissions::ReadOnly => 1,
        SegmentPermissions::ReadExecute => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Program, Segment, encode_program};

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
        encode_program(&Program {
            manifest: &[],
            entry_va: 0x4000_0000,
            stack_size: 0x4000,
            segments: &segments,
        })
        .expect("encode program")
    }

    #[test]
    fn manifest_sizes_reject_truncation_and_segment_overlap() {
        for size in [u32::MAX, 8192, 4096] {
            let mut bytes = program_bytes();
            bytes[8] = 1;
            bytes[27..31].copy_from_slice(&size.to_le_bytes());
            assert!(decode_program(&bytes).is_err(), "manifest size {size}");
        }
    }

    #[test]
    fn old_and_unknown_program_versions_are_rejected() {
        for version in [0u8, 2, u8::MAX] {
            let mut bytes = program_bytes();
            bytes[8] = version;
            assert!(decode_program(&bytes).is_err(), "version {version}");
        }
    }

    #[test]
    fn round_trip_places_segment_bytes_behind_the_first_page() {
        let bytes = program_bytes();
        let program = decode_program(&bytes).expect("decode program");
        let segments: Vec<_> = program.segments().collect();

        assert_eq!(&bytes[..8], &USERLAND_PROGRAM_MAGIC);
        assert_eq!(bytes[8], 1);
        assert_eq!(&bytes[9..17], &0x4000_0000u64.to_le_bytes());
        assert_eq!(&bytes[25..27], &2u16.to_le_bytes());
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
        bytes.resize(
            length + usize::try_from(USERLAND_PAGE_SIZE).expect("page size fits usize"),
            0,
        );

        let program = decode_program(&bytes).expect("slack past the program is not a segment");
        let segments: Vec<_> = program.segments().collect();
        assert_eq!(segments[0].bytes, b"CODE");
        assert_eq!(segments[1].bytes, b"RW");
    }

    #[test]
    fn rejects_foreign_magic() {
        assert_eq!(
            decode_program(b"plain file bytes").expect_err("short bytes are not a program"),
            ProgramDecodeError::BufferTooShort {
                needed: PROGRAM_HEADER_SIZE,
                actual: 16,
            }
        );

        let mut bytes = program_bytes();
        bytes[0] = b'X';
        assert!(matches!(
            decode_program(&bytes),
            Err(ProgramDecodeError::InvalidMagic(_))
        ));
    }

    #[test]
    fn rejects_segment_reaching_past_the_program() {
        let mut bytes = program_bytes();
        let file_size = PROGRAM_HEADER_SIZE + 8;
        bytes[file_size..file_size + 8].copy_from_slice(&u64::MAX.to_le_bytes());

        assert!(matches!(
            decode_program(&bytes),
            Err(ProgramDecodeError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn rejects_unaligned_segment_offset() {
        let mut bytes = program_bytes();
        bytes[PROGRAM_HEADER_SIZE..PROGRAM_HEADER_SIZE + 8]
            .copy_from_slice(&(USERLAND_PAGE_SIZE + 1).to_le_bytes());

        assert_eq!(
            decode_program(&bytes).expect_err("unaligned segment must fail"),
            ProgramDecodeError::InvalidLayout("segment_alignment")
        );
    }

    #[test]
    fn rejects_truncated_descriptors() {
        let mut bytes = program_bytes();
        bytes[25..27].copy_from_slice(&1024u16.to_le_bytes());

        assert_eq!(
            decode_program(&bytes).expect_err("descriptor block must fit a page"),
            ProgramDecodeError::BufferTooShort {
                needed: 31 + 1024 * 40,
                actual: bytes.len()
            }
        );
    }

    #[test]
    fn rejects_semantically_overlapping_segments() {
        let mut bytes = program_bytes();
        let second_va = PROGRAM_HEADER_SIZE + SEGMENT_SIZE + 16;
        bytes[second_va..second_va + 8].copy_from_slice(&0x4000_0000u64.to_le_bytes());

        assert_eq!(
            decode_program(&bytes).expect_err("overlap must fail"),
            ProgramDecodeError::Semantic(UserlandValidationError::OverlappingSegments)
        );
    }

    #[test]
    fn metadata_spans_pages_and_manifest_follows_descriptors() {
        let count = 103usize;
        let segments: Vec<_> = (0..count)
            .map(|index| Segment {
                va_base: 0x4000_0000
                    + u64::try_from(index).expect("segment index fits") * USERLAND_PAGE_SIZE,
                mem_size: USERLAND_PAGE_SIZE,
                permissions: SegmentPermissions::ReadExecute,
                bytes: b"CODE",
            })
            .collect();
        for manifest in [b"record".as_slice(), &[0xAA; 4096]] {
            let bytes = encode_program(&Program {
                manifest,
                entry_va: 0x4000_0000,
                stack_size: 0x4000,
                segments: &segments,
            })
            .expect("metadata spans pages");
            let decoded = decode_program(&bytes).expect("decode metadata");
            assert_eq!(decoded.manifest(), manifest);
            let at = PROGRAM_HEADER_SIZE + count * SEGMENT_SIZE;
            assert_eq!(&bytes[at..at + manifest.len()], manifest);
            assert_eq!(decoded.segments().count(), count);
            let first = decode_segment(&bytes, PROGRAM_HEADER_SIZE).expect("first segment");
            assert_eq!(
                first.file_offset,
                align_up_to_page((at + manifest.len()) as u64).unwrap()
            );
        }
    }
}
