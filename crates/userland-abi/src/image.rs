pub const USERLAND_IMAGE_MAGIC: [u8; 8] = *b"USRLIMG\0";
pub const USERLAND_IMAGE_VERSION: u16 = 1;
pub const USERLAND_IMAGE_HEADER_SIZE: usize = 20;
pub const USERLAND_IMAGE_ENTRY_HEADER_SIZE: usize = 36;
pub const USERLAND_IMAGE_SEGMENT_SIZE: usize = 36;
pub const USERLAND_IMAGE_ENTRY_NAME_CAPACITY: usize = 64;
pub const USERLAND_IMAGE_PAGE_SIZE: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserlandImageHeader {
    pub magic: [u8; 8],
    pub version: u16,
    pub entry_count: u16,
    pub total_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserlandImageEntryHeader {
    pub payload_offset: u64,
    pub payload_size: u64,
    pub entry_va: u64,
    pub stack_size: u64,
    pub segment_count: u16,
    pub name_len: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserlandImageSegment {
    pub file_offset: u64,
    pub file_size: u64,
    pub va_base: u64,
    pub mem_size: u64,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserlandImageError {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserlandImage<'a> {
    bytes: &'a [u8],
    header: UserlandImageHeader,
    entries_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserlandImageEntry<'a> {
    bytes: &'a [u8],
    header: UserlandImageEntryHeader,
    segments_offset: usize,
    name: &'a [u8],
    payload: &'a [u8],
}

pub struct UserlandImageEntries<'a> {
    bytes: &'a [u8],
    next_offset: usize,
    current_index: usize,
    remaining: usize,
    entries_end: usize,
}

pub struct UserlandImageSegments<'a> {
    bytes: &'a [u8],
    next_offset: usize,
    remaining: usize,
}

impl<'a> UserlandImage<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, UserlandImageError> {
        let header = decode_image_header(bytes).ok_or(UserlandImageError::BufferTooShort {
            needed: USERLAND_IMAGE_HEADER_SIZE,
            actual: bytes.len(),
        })?;

        if header.magic != USERLAND_IMAGE_MAGIC {
            return Err(UserlandImageError::InvalidMagic(header.magic));
        }
        if header.version != USERLAND_IMAGE_VERSION {
            return Err(UserlandImageError::InvalidVersion(header.version));
        }
        if header.entry_count == 0 {
            return Err(UserlandImageError::InvalidLayout {
                entry_index: None,
                detail: "entry_count",
            });
        }

        let total_size =
            usize::try_from(header.total_size).map_err(|_| UserlandImageError::InvalidLayout {
                entry_index: None,
                detail: "total_size",
            })?;
        if total_size != bytes.len() {
            return Err(UserlandImageError::InvalidLayout {
                entry_index: None,
                detail: "total_size",
            });
        }

        let mut next_offset = USERLAND_IMAGE_HEADER_SIZE;
        for index in 0..usize::from(header.entry_count) {
            let entry = parse_entry(bytes, next_offset, index, total_size, None)?;
            next_offset = next_offset
                .checked_add(entry_encoded_size(entry.header))
                .ok_or(UserlandImageError::InvalidLayout {
                    entry_index: Some(index),
                    detail: "entry_size_overflow",
                })?;
        }

        let entries_end = next_offset;
        next_offset = USERLAND_IMAGE_HEADER_SIZE;
        for index in 0..usize::from(header.entry_count) {
            let entry = parse_entry(bytes, next_offset, index, total_size, Some(entries_end))?;
            next_offset += entry_encoded_size(entry.header);
        }

        Ok(Self {
            bytes,
            header,
            entries_end,
        })
    }

    pub const fn header(&self) -> &UserlandImageHeader {
        &self.header
    }

    pub fn entry_count(&self) -> usize {
        usize::from(self.header.entry_count)
    }

    pub fn bootstrap_entry(&self) -> UserlandImageEntry<'a> {
        self.entry(0)
            .expect("image always contains bootstrap entry")
    }

    pub fn entry(&self, index: usize) -> Option<UserlandImageEntry<'a>> {
        if index >= self.entry_count() {
            return None;
        }

        let mut next_offset = USERLAND_IMAGE_HEADER_SIZE;
        for current in 0..self.entry_count() {
            let entry = parse_entry(
                self.bytes,
                next_offset,
                current,
                self.bytes.len(),
                Some(self.entries_end),
            )
            .ok()?;
            if current == index {
                return Some(entry);
            }
            next_offset += entry_encoded_size(entry.header);
        }
        None
    }

    pub fn entries(&self) -> UserlandImageEntries<'a> {
        UserlandImageEntries {
            bytes: self.bytes,
            next_offset: USERLAND_IMAGE_HEADER_SIZE,
            current_index: 0,
            remaining: self.entry_count(),
            entries_end: self.entries_end,
        }
    }
}

impl<'a> UserlandImageEntry<'a> {
    pub const fn header(&self) -> &UserlandImageEntryHeader {
        &self.header
    }

    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }

    pub const fn name_bytes(&self) -> &'a [u8] {
        self.name
    }

    pub fn segment_count(&self) -> usize {
        usize::from(self.header.segment_count)
    }

    pub fn segment(&self, index: usize) -> Option<UserlandImageSegment> {
        if index >= self.segment_count() {
            return None;
        }

        let offset = self.segments_offset + index * USERLAND_IMAGE_SEGMENT_SIZE;
        decode_segment(&self.bytes[offset..offset + USERLAND_IMAGE_SEGMENT_SIZE])
    }

    pub fn segments(&self) -> UserlandImageSegments<'a> {
        UserlandImageSegments {
            bytes: self.bytes,
            next_offset: self.segments_offset,
            remaining: self.segment_count(),
        }
    }
}

impl<'a> Iterator for UserlandImageEntries<'a> {
    type Item = UserlandImageEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        let current = parse_entry(
            self.bytes,
            self.next_offset,
            self.current_index,
            self.bytes.len(),
            Some(self.entries_end),
        )
        .ok()?;
        self.next_offset += entry_encoded_size(current.header);
        self.current_index += 1;
        self.remaining -= 1;
        Some(current)
    }
}

impl Iterator for UserlandImageSegments<'_> {
    type Item = UserlandImageSegment;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        let segment = decode_segment(
            self.bytes
                .get(self.next_offset..self.next_offset + USERLAND_IMAGE_SEGMENT_SIZE)?,
        )?;
        self.next_offset += USERLAND_IMAGE_SEGMENT_SIZE;
        self.remaining -= 1;
        Some(segment)
    }
}

pub fn decode_image_header(bytes: &[u8]) -> Option<UserlandImageHeader> {
    if bytes.len() < USERLAND_IMAGE_HEADER_SIZE {
        return None;
    }

    Some(UserlandImageHeader {
        magic: bytes[0..8].try_into().ok()?,
        version: u16::from_le_bytes(bytes[8..10].try_into().ok()?),
        entry_count: u16::from_le_bytes(bytes[10..12].try_into().ok()?),
        total_size: u64::from_le_bytes(bytes[12..20].try_into().ok()?),
    })
}

pub fn decode_entry_header(bytes: &[u8]) -> Option<UserlandImageEntryHeader> {
    if bytes.len() < USERLAND_IMAGE_ENTRY_HEADER_SIZE {
        return None;
    }

    Some(UserlandImageEntryHeader {
        payload_offset: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
        payload_size: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
        entry_va: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
        stack_size: u64::from_le_bytes(bytes[24..32].try_into().ok()?),
        segment_count: u16::from_le_bytes(bytes[32..34].try_into().ok()?),
        name_len: u16::from_le_bytes(bytes[34..36].try_into().ok()?),
    })
}

pub fn decode_segment(bytes: &[u8]) -> Option<UserlandImageSegment> {
    if bytes.len() < USERLAND_IMAGE_SEGMENT_SIZE {
        return None;
    }

    Some(UserlandImageSegment {
        file_offset: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
        file_size: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
        va_base: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
        mem_size: u64::from_le_bytes(bytes[24..32].try_into().ok()?),
        flags: u32::from_le_bytes(bytes[32..36].try_into().ok()?),
    })
}

fn entry_encoded_size(header: UserlandImageEntryHeader) -> usize {
    USERLAND_IMAGE_ENTRY_HEADER_SIZE
        + usize::from(header.segment_count) * USERLAND_IMAGE_SEGMENT_SIZE
        + usize::from(header.name_len)
}

fn parse_entry(
    bytes: &[u8],
    entry_offset: usize,
    entry_index: usize,
    total_size: usize,
    entries_end: Option<usize>,
) -> Result<UserlandImageEntry<'_>, UserlandImageError> {
    let header = decode_entry_header_checked(bytes, entry_offset)?;
    validate_entry_header(header, entry_index)?;
    let segments_offset = entry_offset + USERLAND_IMAGE_ENTRY_HEADER_SIZE;
    let name = parse_entry_name(bytes, header, entry_offset, entry_index, total_size)?;
    let payload = parse_entry_payload(bytes, header, entry_index, total_size, entries_end)?;
    validate_entry_segments(bytes, header, payload, entry_index, segments_offset)?;

    Ok(UserlandImageEntry {
        bytes,
        header,
        segments_offset,
        name,
        payload,
    })
}

fn decode_entry_header_checked(
    bytes: &[u8],
    entry_offset: usize,
) -> Result<UserlandImageEntryHeader, UserlandImageError> {
    decode_entry_header(
        bytes
            .get(entry_offset..)
            .ok_or(UserlandImageError::BufferTooShort {
                needed: entry_offset + USERLAND_IMAGE_ENTRY_HEADER_SIZE,
                actual: bytes.len(),
            })?,
    )
    .ok_or(UserlandImageError::BufferTooShort {
        needed: entry_offset + USERLAND_IMAGE_ENTRY_HEADER_SIZE,
        actual: bytes.len(),
    })
}

fn validate_entry_header(
    header: UserlandImageEntryHeader,
    entry_index: usize,
) -> Result<(), UserlandImageError> {
    if header.segment_count == 0 {
        return Err(invalid_layout(entry_index, "segment_count"));
    }
    if header.name_len == 0 || usize::from(header.name_len) > USERLAND_IMAGE_ENTRY_NAME_CAPACITY {
        return Err(invalid_layout(entry_index, "name_len"));
    }
    if header.stack_size == 0 {
        return Err(invalid_layout(entry_index, "stack_size"));
    }
    Ok(())
}

fn parse_entry_name(
    bytes: &[u8],
    header: UserlandImageEntryHeader,
    entry_offset: usize,
    entry_index: usize,
    total_size: usize,
) -> Result<&[u8], UserlandImageError> {
    let segments_size = usize::from(header.segment_count)
        .checked_mul(USERLAND_IMAGE_SEGMENT_SIZE)
        .ok_or(invalid_layout(entry_index, "segments_size_overflow"))?;
    let name_offset = entry_offset
        .checked_add(USERLAND_IMAGE_ENTRY_HEADER_SIZE)
        .and_then(|offset| offset.checked_add(segments_size))
        .ok_or(invalid_layout(entry_index, "name_offset_overflow"))?;
    let entry_end = name_offset
        .checked_add(usize::from(header.name_len))
        .ok_or(invalid_layout(entry_index, "entry_end_overflow"))?;
    if entry_end > total_size {
        return Err(invalid_layout(entry_index, "entry_bounds"));
    }

    let name = &bytes[name_offset..entry_end];
    if !name.is_ascii() {
        return Err(invalid_layout(entry_index, "name_ascii"));
    }
    Ok(name)
}

fn parse_entry_payload(
    bytes: &[u8],
    header: UserlandImageEntryHeader,
    entry_index: usize,
    total_size: usize,
    entries_end: Option<usize>,
) -> Result<&[u8], UserlandImageError> {
    let payload_offset =
        usize::try_from(header.payload_offset).map_err(|_| out_of_bounds(entry_index, header))?;
    let payload_size =
        usize::try_from(header.payload_size).map_err(|_| out_of_bounds(entry_index, header))?;
    let payload_end = payload_offset
        .checked_add(payload_size)
        .ok_or(out_of_bounds(entry_index, header))?;
    if let Some(entries_end) = entries_end
        && payload_offset < entries_end
    {
        return Err(invalid_layout(entry_index, "payload_overlaps_entries"));
    }
    if payload_end > total_size {
        return Err(out_of_bounds(entry_index, header));
    }
    Ok(&bytes[payload_offset..payload_end])
}

fn validate_entry_segments(
    bytes: &[u8],
    header: UserlandImageEntryHeader,
    payload: &[u8],
    entry_index: usize,
    segments_offset: usize,
) -> Result<(), UserlandImageError> {
    let mut prev_end = header.payload_offset;
    let mut entry_in_executable = false;

    for segment_index in 0..usize::from(header.segment_count) {
        let offset = segments_offset + segment_index * USERLAND_IMAGE_SEGMENT_SIZE;
        let segment = decode_segment(&bytes[offset..offset + USERLAND_IMAGE_SEGMENT_SIZE])
            .expect("segment bytes are inside the validated segment table area");

        validate_segment(segment, header, payload, entry_index, prev_end)?;
        prev_end = segment.file_offset.saturating_add(segment.file_size);
        entry_in_executable |= segment.flags == 2
            && header.entry_va >= segment.va_base
            && header.entry_va < segment.va_base.saturating_add(segment.mem_size);
    }

    if !entry_in_executable {
        return Err(invalid_layout(entry_index, "entry_va"));
    }
    Ok(())
}

fn invalid_layout(entry_index: usize, detail: &'static str) -> UserlandImageError {
    UserlandImageError::InvalidLayout {
        entry_index: Some(entry_index),
        detail,
    }
}

fn out_of_bounds(entry_index: usize, header: UserlandImageEntryHeader) -> UserlandImageError {
    UserlandImageError::OutOfBounds {
        entry_index,
        offset: header.payload_offset,
        size: header.payload_size,
    }
}

fn validate_segment(
    segment: UserlandImageSegment,
    header: UserlandImageEntryHeader,
    payload: &[u8],
    entry_index: usize,
    prev_end: u64,
) -> Result<(), UserlandImageError> {
    if segment.mem_size < segment.file_size {
        return Err(UserlandImageError::InvalidLayout {
            entry_index: Some(entry_index),
            detail: "segment_mem_size",
        });
    }
    if !matches!(segment.flags, 0..=2) {
        return Err(UserlandImageError::InvalidLayout {
            entry_index: Some(entry_index),
            detail: "segment_flags",
        });
    }
    if segment.file_offset % USERLAND_IMAGE_PAGE_SIZE != segment.va_base % USERLAND_IMAGE_PAGE_SIZE
    {
        return Err(UserlandImageError::InvalidLayout {
            entry_index: Some(entry_index),
            detail: "segment_alignment",
        });
    }
    if segment.file_offset < header.payload_offset || segment.file_offset < prev_end {
        return Err(UserlandImageError::InvalidLayout {
            entry_index: Some(entry_index),
            detail: "segment_order",
        });
    }

    let relative_offset = segment
        .file_offset
        .checked_sub(header.payload_offset)
        .ok_or(UserlandImageError::InvalidLayout {
            entry_index: Some(entry_index),
            detail: "segment_order",
        })?;
    let relative_offset =
        usize::try_from(relative_offset).map_err(|_| UserlandImageError::OutOfBounds {
            entry_index,
            offset: segment.file_offset,
            size: segment.file_size,
        })?;
    let file_size =
        usize::try_from(segment.file_size).map_err(|_| UserlandImageError::OutOfBounds {
            entry_index,
            offset: segment.file_offset,
            size: segment.file_size,
        })?;
    let file_end =
        relative_offset
            .checked_add(file_size)
            .ok_or(UserlandImageError::OutOfBounds {
                entry_index,
                offset: segment.file_offset,
                size: segment.file_size,
            })?;
    if file_end > payload.len() {
        return Err(UserlandImageError::OutOfBounds {
            entry_index,
            offset: segment.file_offset,
            size: segment.file_size,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::*;

    #[derive(Clone, Copy)]
    struct TestSegment<'a> {
        va_base: u64,
        mem_size: u64,
        flags: u32,
        bytes: &'a [u8],
    }

    struct TestEntry<'a> {
        name: &'a str,
        entry_va: u64,
        stack_size: u64,
        segments: &'a [TestSegment<'a>],
    }

    #[test]
    fn round_trip_parse_preserves_segment_layout() {
        let image_bytes = build_image(&[
            TestEntry {
                name: "rootkeeper",
                entry_va: 0x4000_0000,
                stack_size: 0x4000,
                segments: &[
                    TestSegment {
                        va_base: 0x4000_0000,
                        mem_size: 0x1000,
                        flags: 2,
                        bytes: b"CODE",
                    },
                    TestSegment {
                        va_base: 0x4000_1000,
                        mem_size: 0x1000,
                        flags: 0,
                        bytes: b"RW",
                    },
                ],
            },
            TestEntry {
                name: "shell",
                entry_va: 0x5000_0000,
                stack_size: 0x8000,
                segments: &[TestSegment {
                    va_base: 0x5000_0000,
                    mem_size: 0x2000,
                    flags: 2,
                    bytes: b"ELF",
                }],
            },
        ]);

        let image = UserlandImage::parse(&image_bytes).expect("image should parse");
        assert_eq!(image.entry_count(), 2);
        assert_eq!(image.bootstrap_entry().name_bytes(), b"rootkeeper");

        let first = image.entry(0).expect("first entry");
        assert_eq!(first.header().segment_count, 2);
        assert_eq!(first.header().entry_va, 0x4000_0000);
        let segments: Vec<UserlandImageSegment> = first.segments().collect();
        assert_eq!(
            segments,
            vec![
                UserlandImageSegment {
                    file_offset: segments[0].file_offset,
                    file_size: 4,
                    va_base: 0x4000_0000,
                    mem_size: 0x1000,
                    flags: 2,
                },
                UserlandImageSegment {
                    file_offset: segments[1].file_offset,
                    file_size: 2,
                    va_base: 0x4000_1000,
                    mem_size: 0x1000,
                    flags: 0,
                }
            ]
        );
        assert_eq!(
            &image_bytes[usize::try_from(segments[0].file_offset).unwrap()
                ..usize::try_from(segments[0].file_offset).unwrap() + 4],
            b"CODE"
        );
        assert_eq!(
            &image_bytes[usize::try_from(segments[1].file_offset).unwrap()
                ..usize::try_from(segments[1].file_offset).unwrap() + 2],
            b"RW"
        );
    }

    #[test]
    fn rejects_segment_file_offset_outside_blob() {
        let mut image_bytes = build_image(&[TestEntry {
            name: "rootkeeper",
            entry_va: 0x4000_0000,
            stack_size: 0x4000,
            segments: &[TestSegment {
                va_base: 0x4000_0000,
                mem_size: 0x1000,
                flags: 2,
                bytes: b"CODE",
            }],
        }]);

        let segment_offset = USERLAND_IMAGE_HEADER_SIZE + USERLAND_IMAGE_ENTRY_HEADER_SIZE;
        let bad_offset =
            ((image_bytes.len() as u64 / USERLAND_IMAGE_PAGE_SIZE) + 1) * USERLAND_IMAGE_PAGE_SIZE;
        image_bytes[segment_offset..segment_offset + 8].copy_from_slice(&bad_offset.to_le_bytes());

        let err = UserlandImage::parse(&image_bytes).expect_err("segment bounds must fail");
        assert!(matches!(
            err,
            UserlandImageError::OutOfBounds { entry_index: 0, .. }
        ));
    }

    #[test]
    fn bootstrap_is_always_first_entry() {
        let image_bytes = build_image(&[
            TestEntry {
                name: "rootkeeper",
                entry_va: 0x4000_0000,
                stack_size: 0x4000,
                segments: &[TestSegment {
                    va_base: 0x4000_0000,
                    mem_size: 0x1000,
                    flags: 2,
                    bytes: b"BOOT",
                }],
            },
            TestEntry {
                name: "service",
                entry_va: 0x5000_0000,
                stack_size: 0x4000,
                segments: &[TestSegment {
                    va_base: 0x5000_0000,
                    mem_size: 0x1000,
                    flags: 2,
                    bytes: b"SVC",
                }],
            },
        ]);

        let image = UserlandImage::parse(&image_bytes).expect("image should parse");
        assert_eq!(image.bootstrap_entry().name_bytes(), b"rootkeeper");
        assert_eq!(
            image.entry(1).expect("second entry").name_bytes(),
            b"service"
        );
    }

    fn build_image(entries: &[TestEntry<'_>]) -> Vec<u8> {
        let metadata_size = USERLAND_IMAGE_HEADER_SIZE
            + entries
                .iter()
                .map(|entry| {
                    USERLAND_IMAGE_ENTRY_HEADER_SIZE
                        + entry.segments.len() * USERLAND_IMAGE_SEGMENT_SIZE
                        + entry.name.len()
                })
                .sum::<usize>();

        let mut payload_cursor = metadata_size as u64;
        let mut encoded_entries = Vec::new();
        let mut payloads = Vec::new();

        for entry in entries {
            let payload_offset = payload_cursor;
            let mut payload = Vec::new();
            let mut encoded_segments = Vec::new();

            for segment in entry.segments {
                let file_offset =
                    align_file_offset(payload_offset + payload.len() as u64, segment.va_base);
                let pad = usize::try_from(file_offset - (payload_offset + payload.len() as u64))
                    .expect("padding fits in usize");
                payload.resize(payload.len() + pad, 0);
                payload.extend_from_slice(segment.bytes);

                let mut bytes = Vec::with_capacity(USERLAND_IMAGE_SEGMENT_SIZE);
                bytes.extend_from_slice(&file_offset.to_le_bytes());
                bytes.extend_from_slice(&(segment.bytes.len() as u64).to_le_bytes());
                bytes.extend_from_slice(&segment.va_base.to_le_bytes());
                bytes.extend_from_slice(&segment.mem_size.to_le_bytes());
                bytes.extend_from_slice(&segment.flags.to_le_bytes());
                encoded_segments.extend_from_slice(&bytes);
            }

            let payload_size = payload.len() as u64;
            payload_cursor += payload_size;

            let mut encoded_entry = Vec::new();
            encoded_entry.extend_from_slice(&payload_offset.to_le_bytes());
            encoded_entry.extend_from_slice(&payload_size.to_le_bytes());
            encoded_entry.extend_from_slice(&entry.entry_va.to_le_bytes());
            encoded_entry.extend_from_slice(&entry.stack_size.to_le_bytes());
            encoded_entry.extend_from_slice(&(entry.segments.len() as u16).to_le_bytes());
            encoded_entry.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
            encoded_entry.extend_from_slice(&encoded_segments);
            encoded_entry.extend_from_slice(entry.name.as_bytes());

            encoded_entries.push(encoded_entry);
            payloads.push(payload);
        }

        let total_size = usize::try_from(payload_cursor).expect("image fits in usize");
        let mut image = Vec::with_capacity(total_size);
        image.extend_from_slice(&USERLAND_IMAGE_MAGIC);
        image.extend_from_slice(&USERLAND_IMAGE_VERSION.to_le_bytes());
        image.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        image.extend_from_slice(&(total_size as u64).to_le_bytes());

        for entry in encoded_entries {
            image.extend_from_slice(&entry);
        }
        for payload in payloads {
            image.extend_from_slice(&payload);
        }

        image
    }

    fn align_file_offset(offset: u64, va_base: u64) -> u64 {
        let target = va_base % USERLAND_IMAGE_PAGE_SIZE;
        let current = offset % USERLAND_IMAGE_PAGE_SIZE;
        if current <= target {
            offset + (target - current)
        } else {
            offset + (USERLAND_IMAGE_PAGE_SIZE - (current - target))
        }
    }
}
