//! Сериализатор `userland.img` - единственный writer формата, парный
//! [`UserlandImage::parse`](crate::UserlandImage::parse).

use alloc::vec::Vec;

use crate::image::{
    USERLAND_IMAGE_ENTRY_HEADER_SIZE, USERLAND_IMAGE_HEADER_SIZE, USERLAND_IMAGE_MAGIC,
    USERLAND_IMAGE_PAGE_SIZE, USERLAND_IMAGE_SEGMENT_SIZE, USERLAND_IMAGE_VERSION,
};

/// Сегмент-вход сборщика образа (`file_size` берётся из длины `bytes`).
pub struct ImageSegmentInput<'a> {
    pub va_base: u64,
    pub mem_size: u64,
    pub flags: u32,
    pub bytes: &'a [u8],
}

/// Entry-вход сборщика образа. Сегменты сериализуются в порядке среза.
pub struct ImageEntryInput<'a> {
    pub name: &'a str,
    pub entry_va: u64,
    pub stack_size: u64,
    pub segments: &'a [ImageSegmentInput<'a>],
}

/// Ошибки сериализации образа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBuildError {
    /// Число entry не помещается в `u16`.
    TooManyEntries,
    /// Число сегментов entry не помещается в `u16`.
    TooManySegments,
    /// Длина имени entry не помещается в `u16`.
    NameTooLong,
    /// Размер blob-а или промежуточный offset переполняет адресацию.
    SizeOverflow,
}

/// Выравнивает `offset` до конгруэнтности `va_base` по модулю страницы.
#[must_use]
pub fn align_file_offset(offset: u64, va_base: u64) -> u64 {
    let target = va_base % USERLAND_IMAGE_PAGE_SIZE;
    let current = offset % USERLAND_IMAGE_PAGE_SIZE;
    if current <= target {
        offset + (target - current)
    } else {
        offset + (USERLAND_IMAGE_PAGE_SIZE - (current - target))
    }
}

/// Сериализует образ в blob, который примет
/// [`UserlandImage::parse`](crate::UserlandImage::parse): header, затем
/// entry-записи в порядке среза, затем payload-байты.
pub fn build_userland_image(entries: &[ImageEntryInput<'_>]) -> Result<Vec<u8>, ImageBuildError> {
    let mut metadata_size = USERLAND_IMAGE_HEADER_SIZE;
    for entry in entries {
        metadata_size = metadata_size
            .checked_add(encoded_entry_size(entry))
            .ok_or(ImageBuildError::SizeOverflow)?;
    }

    let mut payload_cursor =
        u64::try_from(metadata_size).map_err(|_| ImageBuildError::SizeOverflow)?;
    let mut encoded_entries = Vec::with_capacity(entries.len());
    let mut payloads = Vec::with_capacity(entries.len());

    for entry in entries {
        let payload_offset = payload_cursor;
        let mut payload = Vec::new();
        let mut encoded_segments =
            Vec::with_capacity(entry.segments.len() * USERLAND_IMAGE_SEGMENT_SIZE);

        // offset-ы абсолютны внутри финального blob-а.
        for segment in entry.segments {
            let cursor = payload_offset
                .checked_add(payload.len() as u64)
                .ok_or(ImageBuildError::SizeOverflow)?;
            let file_offset = align_file_offset(cursor, segment.va_base);
            let pad =
                usize::try_from(file_offset - cursor).map_err(|_| ImageBuildError::SizeOverflow)?;
            payload.resize(payload.len() + pad, 0);
            payload.extend_from_slice(segment.bytes);

            let file_size =
                u64::try_from(segment.bytes.len()).map_err(|_| ImageBuildError::SizeOverflow)?;
            encoded_segments.extend_from_slice(&file_offset.to_le_bytes());
            encoded_segments.extend_from_slice(&file_size.to_le_bytes());
            encoded_segments.extend_from_slice(&segment.va_base.to_le_bytes());
            encoded_segments.extend_from_slice(&segment.mem_size.to_le_bytes());
            encoded_segments.extend_from_slice(&segment.flags.to_le_bytes());
        }

        let payload_size =
            u64::try_from(payload.len()).map_err(|_| ImageBuildError::SizeOverflow)?;
        payload_cursor = payload_cursor
            .checked_add(payload_size)
            .ok_or(ImageBuildError::SizeOverflow)?;

        let segment_count =
            u16::try_from(entry.segments.len()).map_err(|_| ImageBuildError::TooManySegments)?;
        let name_len = u16::try_from(entry.name.len()).map_err(|_| ImageBuildError::NameTooLong)?;

        let mut encoded_entry = Vec::with_capacity(encoded_entry_size(entry));
        encoded_entry.extend_from_slice(&payload_offset.to_le_bytes());
        encoded_entry.extend_from_slice(&payload_size.to_le_bytes());
        encoded_entry.extend_from_slice(&entry.entry_va.to_le_bytes());
        encoded_entry.extend_from_slice(&entry.stack_size.to_le_bytes());
        encoded_entry.extend_from_slice(&segment_count.to_le_bytes());
        encoded_entry.extend_from_slice(&name_len.to_le_bytes());
        encoded_entry.extend_from_slice(&encoded_segments);
        encoded_entry.extend_from_slice(entry.name.as_bytes());

        encoded_entries.push(encoded_entry);
        payloads.push(payload);
    }

    let total_size = usize::try_from(payload_cursor).map_err(|_| ImageBuildError::SizeOverflow)?;
    let entry_count = u16::try_from(entries.len()).map_err(|_| ImageBuildError::TooManyEntries)?;

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

fn encoded_entry_size(entry: &ImageEntryInput<'_>) -> usize {
    USERLAND_IMAGE_ENTRY_HEADER_SIZE
        + entry.segments.len() * USERLAND_IMAGE_SEGMENT_SIZE
        + entry.name.len()
}
