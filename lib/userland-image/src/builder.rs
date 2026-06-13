use alloc::vec::Vec;

use userland::{
    EntryView, SegmentPermissions, SegmentView, UserlandValidationError, validate_entry,
};

use crate::image::{
    ENTRY_HEADER_SIZE, IMAGE_HEADER_SIZE, SEGMENT_SIZE, USERLAND_IMAGE_MAGIC,
    USERLAND_IMAGE_VERSION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageEncodeError {
    NoEntries,
    TooManyEntries,
    TooManySegments {
        entry_index: usize,
    },
    SizeOverflow,
    Semantic {
        entry_index: usize,
        source: UserlandValidationError,
    },
}

pub fn encode<'a, E: EntryView<'a>>(entries: &[E]) -> Result<Vec<u8>, ImageEncodeError> {
    if entries.is_empty() {
        return Err(ImageEncodeError::NoEntries);
    }
    let entry_count = u16::try_from(entries.len()).map_err(|_| ImageEncodeError::TooManyEntries)?;
    let mut metadata_size = IMAGE_HEADER_SIZE;

    for (entry_index, entry) in entries.iter().enumerate() {
        validate_entry(entry).map_err(|source| ImageEncodeError::Semantic {
            entry_index,
            source,
        })?;
        let segment_count = entry.segments().count();
        u16::try_from(segment_count)
            .map_err(|_| ImageEncodeError::TooManySegments { entry_index })?;
        metadata_size = metadata_size
            .checked_add(ENTRY_HEADER_SIZE)
            .and_then(|size| size.checked_add(segment_count.checked_mul(SEGMENT_SIZE)?))
            .and_then(|size| size.checked_add(entry.name().len()))
            .ok_or(ImageEncodeError::SizeOverflow)?;
    }

    let mut payload_cursor =
        u64::try_from(metadata_size).map_err(|_| ImageEncodeError::SizeOverflow)?;
    let mut encoded_entries = Vec::with_capacity(entries.len());
    let mut payloads = Vec::with_capacity(entries.len());

    for (entry_index, entry) in entries.iter().enumerate() {
        let payload_offset = payload_cursor;
        let mut payload = Vec::new();
        let segment_count = entry.segments().count();
        let mut encoded_segments = Vec::with_capacity(segment_count * SEGMENT_SIZE);

        for segment in entry.segments() {
            let cursor = payload_offset
                .checked_add(
                    u64::try_from(payload.len()).map_err(|_| ImageEncodeError::SizeOverflow)?,
                )
                .ok_or(ImageEncodeError::SizeOverflow)?;
            let file_offset = align_file_offset(cursor, segment.va_base());
            let padding = usize::try_from(file_offset - cursor)
                .map_err(|_| ImageEncodeError::SizeOverflow)?;
            payload.resize(
                payload
                    .len()
                    .checked_add(padding)
                    .ok_or(ImageEncodeError::SizeOverflow)?,
                0,
            );
            payload.extend_from_slice(segment.bytes());

            let file_size =
                u64::try_from(segment.bytes().len()).map_err(|_| ImageEncodeError::SizeOverflow)?;
            encoded_segments.extend_from_slice(&file_offset.to_le_bytes());
            encoded_segments.extend_from_slice(&file_size.to_le_bytes());
            encoded_segments.extend_from_slice(&segment.va_base().to_le_bytes());
            encoded_segments.extend_from_slice(&segment.mem_size().to_le_bytes());
            encoded_segments
                .extend_from_slice(&encode_permissions(segment.permissions()).to_le_bytes());
        }

        let payload_size =
            u64::try_from(payload.len()).map_err(|_| ImageEncodeError::SizeOverflow)?;
        payload_cursor = payload_cursor
            .checked_add(payload_size)
            .ok_or(ImageEncodeError::SizeOverflow)?;

        let segment_count = u16::try_from(segment_count)
            .map_err(|_| ImageEncodeError::TooManySegments { entry_index })?;
        let name_len =
            u16::try_from(entry.name().len()).map_err(|_| ImageEncodeError::SizeOverflow)?;
        let entry_capacity = ENTRY_HEADER_SIZE
            .checked_add(
                usize::from(segment_count)
                    .checked_mul(SEGMENT_SIZE)
                    .ok_or(ImageEncodeError::SizeOverflow)?,
            )
            .and_then(|size| size.checked_add(entry.name().len()))
            .ok_or(ImageEncodeError::SizeOverflow)?;
        let mut encoded_entry = Vec::with_capacity(entry_capacity);
        encoded_entry.extend_from_slice(&payload_offset.to_le_bytes());
        encoded_entry.extend_from_slice(&payload_size.to_le_bytes());
        encoded_entry.extend_from_slice(&entry.entry_va().to_le_bytes());
        encoded_entry.extend_from_slice(&entry.stack_size().to_le_bytes());
        encoded_entry.extend_from_slice(&segment_count.to_le_bytes());
        encoded_entry.extend_from_slice(&name_len.to_le_bytes());
        encoded_entry.extend_from_slice(&encoded_segments);
        encoded_entry.extend_from_slice(entry.name().as_bytes());

        encoded_entries.push(encoded_entry);
        payloads.push(payload);
    }

    let total_size = usize::try_from(payload_cursor).map_err(|_| ImageEncodeError::SizeOverflow)?;
    let mut image = Vec::with_capacity(total_size);
    image.extend_from_slice(&USERLAND_IMAGE_MAGIC);
    image.extend_from_slice(&USERLAND_IMAGE_VERSION.to_le_bytes());
    image.extend_from_slice(&entry_count.to_le_bytes());
    image.extend_from_slice(&payload_cursor.to_le_bytes());
    for entry in encoded_entries {
        image.extend_from_slice(&entry);
    }
    for payload in payloads {
        image.extend_from_slice(&payload);
    }
    Ok(image)
}

fn align_file_offset(offset: u64, va_base: u64) -> u64 {
    let target = va_base % userland::USERLAND_PAGE_SIZE;
    let current = offset % userland::USERLAND_PAGE_SIZE;
    if current <= target {
        offset + (target - current)
    } else {
        offset + (userland::USERLAND_PAGE_SIZE - (current - target))
    }
}

const fn encode_permissions(permissions: SegmentPermissions) -> u32 {
    match permissions {
        SegmentPermissions::ReadWrite => 0,
        SegmentPermissions::ReadOnly => 1,
        SegmentPermissions::ReadExecute => 2,
    }
}
