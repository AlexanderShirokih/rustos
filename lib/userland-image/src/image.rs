use userland::{EntryView, Segment, SegmentPermissions, UserlandValidationError, validate_entry};

pub const USERLAND_IMAGE_MAGIC: [u8; 8] = *b"USRLIMG\0";
pub const USERLAND_IMAGE_VERSION: u16 = 1;
pub(crate) const IMAGE_HEADER_SIZE: usize = 20;
pub(crate) const ENTRY_HEADER_SIZE: usize = 36;
pub(crate) const SEGMENT_SIZE: usize = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImageHeader {
    magic: [u8; 8],
    version: u16,
    entry_count: u16,
    total_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EntryHeader {
    payload_offset: u64,
    payload_size: u64,
    entry_va: u64,
    stack_size: u64,
    segment_count: u16,
    name_len: u16,
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
    Semantic {
        entry_index: usize,
        source: UserlandValidationError,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct DecodedImage<'a> {
    bytes: &'a [u8],
    header: ImageHeader,
    entries_end: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct DecodedEntry<'a> {
    bytes: &'a [u8],
    header: EntryHeader,
    segments_offset: usize,
    name: &'a str,
}

pub struct DecodedEntries<'a> {
    image: DecodedImage<'a>,
    next_offset: usize,
    current_index: usize,
}

pub struct DecodedSegments<'a> {
    bytes: &'a [u8],
    next_offset: usize,
    remaining: usize,
}

/// Декодирует образ из буфера. Длина буфера должна быть не меньше размера, объявленного в заголовке.
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

        let mut next_offset = IMAGE_HEADER_SIZE;
        for index in 0..usize::from(header.entry_count) {
            let entry_header = decode_entry_header_checked(bytes, next_offset)?;
            let encoded_size = entry_encoded_size(entry_header, index)?;
            next_offset = next_offset
                .checked_add(encoded_size)
                .ok_or(invalid_layout(Some(index), "entry_size_overflow"))?;
            if next_offset > total_size {
                return Err(invalid_layout(Some(index), "entry_bounds"));
            }
        }
        let entries_end = next_offset;

        next_offset = IMAGE_HEADER_SIZE;
        for index in 0..usize::from(header.entry_count) {
            let entry = parse_entry(bytes, next_offset, index, entries_end)?;
            validate_entry(&entry).map_err(|source| ImageDecodeError::Semantic {
                entry_index: index,
                source,
            })?;
            next_offset += entry_encoded_size(entry.header, index)?;
        }
        Ok(Self {
            bytes,
            header,
            entries_end,
        })
    }

    pub fn entry_count(&self) -> usize {
        usize::from(self.header.entry_count)
    }

    pub fn bootstrap_entry(&self) -> DecodedEntry<'a> {
        self.entries()
            .next()
            .expect("decoded image contains an entry")
    }

    /// Возвращает entry с заданным именем.
    pub fn entry(&self, name: &str) -> Option<DecodedEntry<'a>> {
        self.entries().find(|entry| entry.name() == name)
    }

    pub fn entries(&self) -> DecodedEntries<'a> {
        DecodedEntries {
            image: *self,
            next_offset: IMAGE_HEADER_SIZE,
            current_index: 0,
        }
    }
}

impl<'a> EntryView<'a> for DecodedEntry<'a> {
    type Segment = Segment<'a>;

    fn name(&self) -> &'a str {
        self.name
    }

    fn entry_va(&self) -> u64 {
        self.header.entry_va
    }

    fn stack_size(&self) -> u64 {
        self.header.stack_size
    }

    fn segments(&self) -> impl Iterator<Item = Self::Segment> + '_ {
        DecodedSegments {
            bytes: self.bytes,
            next_offset: self.segments_offset,
            remaining: usize::from(self.header.segment_count),
        }
    }
}

impl<'a> Iterator for DecodedEntries<'a> {
    type Item = DecodedEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_index == self.image.entry_count() {
            return None;
        }
        let entry = parse_entry(
            self.image.bytes,
            self.next_offset,
            self.current_index,
            self.image.entries_end,
        )
        .expect("entries were validated during decode");
        self.next_offset +=
            entry_encoded_size(entry.header, self.current_index).expect("validated entry size");
        self.current_index += 1;
        Some(entry)
    }
}

impl<'a> Iterator for DecodedSegments<'a> {
    type Item = Segment<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let wire = decode_segment(
            self.bytes
                .get(self.next_offset..self.next_offset + SEGMENT_SIZE)?,
        )?;
        self.next_offset += SEGMENT_SIZE;
        self.remaining -= 1;

        let start = usize::try_from(wire.file_offset).ok()?;
        let size = usize::try_from(wire.file_size).ok()?;
        let bytes = self.bytes.get(start..start.checked_add(size)?)?;
        Some(Segment {
            va_base: wire.va_base,
            mem_size: wire.mem_size,
            permissions: decode_permissions(wire.flags)?,
            bytes,
        })
    }
}

fn parse_entry(
    bytes: &[u8],
    entry_offset: usize,
    entry_index: usize,
    entries_end: usize,
) -> Result<DecodedEntry<'_>, ImageDecodeError> {
    let header = decode_entry_header_checked(bytes, entry_offset)?;
    let segments_size = usize::from(header.segment_count)
        .checked_mul(SEGMENT_SIZE)
        .ok_or(invalid_layout(Some(entry_index), "segments_size_overflow"))?;
    let segments_offset = entry_offset
        .checked_add(ENTRY_HEADER_SIZE)
        .ok_or(invalid_layout(
            Some(entry_index),
            "segments_offset_overflow",
        ))?;
    let name_offset = segments_offset
        .checked_add(segments_size)
        .ok_or(invalid_layout(Some(entry_index), "name_offset_overflow"))?;
    let name_end = name_offset
        .checked_add(usize::from(header.name_len))
        .ok_or(invalid_layout(Some(entry_index), "name_end_overflow"))?;
    let name_bytes = bytes
        .get(name_offset..name_end)
        .ok_or(invalid_layout(Some(entry_index), "entry_bounds"))?;
    let name = core::str::from_utf8(name_bytes)
        .map_err(|_| invalid_layout(Some(entry_index), "name_utf8"))?;

    validate_payload_bounds(bytes, header, entry_index, entries_end)?;
    validate_wire_segments(bytes, header, entry_index, segments_offset)?;

    Ok(DecodedEntry {
        bytes,
        header,
        segments_offset,
        name,
    })
}

fn validate_payload_bounds(
    bytes: &[u8],
    header: EntryHeader,
    entry_index: usize,
    entries_end: usize,
) -> Result<(), ImageDecodeError> {
    let offset =
        usize::try_from(header.payload_offset).map_err(|_| out_of_bounds(entry_index, header))?;
    let size =
        usize::try_from(header.payload_size).map_err(|_| out_of_bounds(entry_index, header))?;
    let end = offset
        .checked_add(size)
        .ok_or(out_of_bounds(entry_index, header))?;
    if offset < entries_end {
        return Err(invalid_layout(
            Some(entry_index),
            "payload_overlaps_entries",
        ));
    }
    if end > bytes.len() {
        return Err(out_of_bounds(entry_index, header));
    }
    Ok(())
}

fn validate_wire_segments(
    bytes: &[u8],
    header: EntryHeader,
    entry_index: usize,
    segments_offset: usize,
) -> Result<(), ImageDecodeError> {
    let payload_end = header
        .payload_offset
        .checked_add(header.payload_size)
        .ok_or(out_of_bounds(entry_index, header))?;
    let mut previous_end = header.payload_offset;

    for segment_index in 0..usize::from(header.segment_count) {
        let offset = segments_offset
            .checked_add(
                segment_index
                    .checked_mul(SEGMENT_SIZE)
                    .ok_or(invalid_layout(Some(entry_index), "segment_offset_overflow"))?,
            )
            .ok_or(invalid_layout(Some(entry_index), "segment_offset_overflow"))?;
        let segment = decode_segment(
            bytes
                .get(offset..offset + SEGMENT_SIZE)
                .ok_or(invalid_layout(Some(entry_index), "segment_bounds"))?,
        )
        .ok_or(invalid_layout(Some(entry_index), "segment_bounds"))?;

        if decode_permissions(segment.flags).is_none() {
            return Err(invalid_layout(Some(entry_index), "segment_flags"));
        }
        if segment.file_offset % userland::USERLAND_PAGE_SIZE
            != segment.va_base % userland::USERLAND_PAGE_SIZE
        {
            return Err(invalid_layout(Some(entry_index), "segment_alignment"));
        }
        let segment_end = segment.file_offset.checked_add(segment.file_size).ok_or(
            ImageDecodeError::OutOfBounds {
                entry_index,
                offset: segment.file_offset,
                size: segment.file_size,
            },
        )?;
        if segment.file_offset < previous_end
            || segment.file_offset < header.payload_offset
            || segment_end > payload_end
        {
            return Err(ImageDecodeError::OutOfBounds {
                entry_index,
                offset: segment.file_offset,
                size: segment.file_size,
            });
        }
        previous_end = segment_end;
    }
    Ok(())
}

fn decode_image_header(bytes: &[u8]) -> Option<ImageHeader> {
    Some(ImageHeader {
        magic: bytes.get(0..8)?.try_into().ok()?,
        version: u16::from_le_bytes(bytes.get(8..10)?.try_into().ok()?),
        entry_count: u16::from_le_bytes(bytes.get(10..12)?.try_into().ok()?),
        total_size: u64::from_le_bytes(bytes.get(12..20)?.try_into().ok()?),
    })
}

fn decode_entry_header_checked(
    bytes: &[u8],
    offset: usize,
) -> Result<EntryHeader, ImageDecodeError> {
    decode_entry_header(bytes.get(offset..).unwrap_or_default()).ok_or(
        ImageDecodeError::BufferTooShort {
            needed: offset.saturating_add(ENTRY_HEADER_SIZE),
            actual: bytes.len(),
        },
    )
}

fn decode_entry_header(bytes: &[u8]) -> Option<EntryHeader> {
    Some(EntryHeader {
        payload_offset: u64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?),
        payload_size: u64::from_le_bytes(bytes.get(8..16)?.try_into().ok()?),
        entry_va: u64::from_le_bytes(bytes.get(16..24)?.try_into().ok()?),
        stack_size: u64::from_le_bytes(bytes.get(24..32)?.try_into().ok()?),
        segment_count: u16::from_le_bytes(bytes.get(32..34)?.try_into().ok()?),
        name_len: u16::from_le_bytes(bytes.get(34..36)?.try_into().ok()?),
    })
}

fn decode_segment(bytes: &[u8]) -> Option<WireSegment> {
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

fn entry_encoded_size(header: EntryHeader, entry_index: usize) -> Result<usize, ImageDecodeError> {
    usize::from(header.segment_count)
        .checked_mul(SEGMENT_SIZE)
        .and_then(|size| size.checked_add(ENTRY_HEADER_SIZE))
        .and_then(|size| size.checked_add(usize::from(header.name_len)))
        .ok_or(invalid_layout(Some(entry_index), "entry_size_overflow"))
}

fn invalid_layout(entry_index: Option<usize>, detail: &'static str) -> ImageDecodeError {
    ImageDecodeError::InvalidLayout {
        entry_index,
        detail,
    }
}

fn out_of_bounds(entry_index: usize, header: EntryHeader) -> ImageDecodeError {
    ImageDecodeError::OutOfBounds {
        entry_index,
        offset: header.payload_offset,
        size: header.payload_size,
    }
}

#[cfg(test)]
mod tests {
    use userland::{Entry, EntryView, Segment, SegmentPermissions};

    use super::*;
    use crate::encode;

    fn image_bytes() -> alloc::vec::Vec<u8> {
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
        encode(&[Entry {
            name: "rootkeeper",
            entry_va: 0x4000_0000,
            stack_size: 0x4000,
            segments: &segments,
        }])
        .expect("encode")
    }

    #[test]
    fn round_trip_exposes_semantic_segments() {
        let bytes = image_bytes();
        let image = decode(&bytes).expect("decode");
        let entry = image.bootstrap_entry();
        let segments: alloc::vec::Vec<_> = entry.segments().collect();

        assert_eq!(entry.name(), "rootkeeper");
        assert_eq!(segments[0].bytes, b"CODE");
        assert_eq!(segments[1].permissions, SegmentPermissions::ReadWrite);
    }

    #[test]
    fn decode_tolerates_trailing_padding() {
        let mut bytes = image_bytes();
        bytes.extend(core::iter::repeat(0u8).take(4096));

        let image = decode(&bytes).expect("padded buffer decodes");
        let entry = image.bootstrap_entry();
        assert_eq!(entry.name(), "rootkeeper");
        assert_eq!(entry.segments().next().expect("segment").bytes, b"CODE");
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
    fn rejects_segment_outside_payload() {
        let mut bytes = image_bytes();
        let segment_offset = IMAGE_HEADER_SIZE + ENTRY_HEADER_SIZE;
        let aligned_out_of_bounds = u64::MAX - (userland::USERLAND_PAGE_SIZE - 1);
        bytes[segment_offset..segment_offset + 8]
            .copy_from_slice(&aligned_out_of_bounds.to_le_bytes());

        assert!(matches!(
            decode(&bytes),
            Err(ImageDecodeError::OutOfBounds { entry_index: 0, .. })
        ));
    }

    #[test]
    fn rejects_semantically_overlapping_segments() {
        let mut bytes = image_bytes();
        let second_segment = IMAGE_HEADER_SIZE + ENTRY_HEADER_SIZE + SEGMENT_SIZE;
        bytes[second_segment + 16..second_segment + 24]
            .copy_from_slice(&0x4000_0000u64.to_le_bytes());

        assert_eq!(
            decode(&bytes).expect_err("overlap must fail"),
            ImageDecodeError::Semantic {
                entry_index: 0,
                source: UserlandValidationError::OverlappingSegments,
            }
        );
    }
}
