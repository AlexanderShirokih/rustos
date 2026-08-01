use alloc::{collections::BTreeMap, vec::Vec};

use userland::{
    EntryView, SegmentPermissions, SegmentView, USERLAND_PAGE_SIZE, UserlandValidationError,
    validate_entry,
};

use crate::{
    image::{IMAGE_HEADER_SIZE, RECORD_SIZE, USERLAND_IMAGE_MAGIC, USERLAND_IMAGE_VERSION},
    program::{PROGRAM_HEADER_SIZE, SEGMENT_SIZE, USERLAND_PROGRAM_MAGIC},
};

/// Запись, укладываемая в образ: путь и уже готовые байты содержимого.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageRecord<'a> {
    pub path: &'a str,
    pub data: &'a [u8],
}

// Ноль в `alias_of` означает собственное окно, ненулевое значение - номер
// записи, чьё окно повторено, плюс один.
struct DataWindow {
    offset: u64,
    size: u64,
    alias_of: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageEncodeError {
    NoRecords,
    TooManyRecords,
    TooManySegments,
    PathTooLong,
    DuplicatePath,
    BootRecordMissing,
    SizeOverflow,
    Semantic(UserlandValidationError),
}

/// Собирает образ: заголовок, таблица записей в порядке пути, байты путей и
/// данные, выровненные на [`USERLAND_PAGE_SIZE`]. Записи с одинаковым
/// содержимым делят одно окно данных. `boot_path` задаёт boot-запись.
pub fn encode(records: &[ImageRecord<'_>], boot_path: &str) -> Result<Vec<u8>, ImageEncodeError> {
    if records.is_empty() {
        return Err(ImageEncodeError::NoRecords);
    }
    let entry_count = u16::try_from(records.len()).map_err(|_| ImageEncodeError::TooManyRecords)?;

    let mut sorted: Vec<&ImageRecord<'_>> = records.iter().collect();
    sorted.sort_by(|left, right| left.path.cmp(right.path));
    if sorted.windows(2).any(|pair| pair[0].path == pair[1].path) {
        return Err(ImageEncodeError::DuplicatePath);
    }
    let boot_index = sorted
        .iter()
        .position(|record| record.path == boot_path)
        .ok_or(ImageEncodeError::BootRecordMissing)?;
    let boot_index = u16::try_from(boot_index).map_err(|_| ImageEncodeError::TooManyRecords)?;

    let mut path_cursor = u64::try_from(
        usize::from(entry_count)
            .checked_mul(RECORD_SIZE)
            .and_then(|table| table.checked_add(IMAGE_HEADER_SIZE))
            .ok_or(ImageEncodeError::SizeOverflow)?,
    )
    .map_err(|_| ImageEncodeError::SizeOverflow)?;
    let mut paths = Vec::new();
    let mut table = Vec::with_capacity(sorted.len());

    for record in &sorted {
        let path_len =
            u16::try_from(record.path.len()).map_err(|_| ImageEncodeError::PathTooLong)?;
        table.push((path_cursor, path_len));
        paths.extend_from_slice(record.path.as_bytes());
        path_cursor = path_cursor
            .checked_add(u64::from(path_len))
            .ok_or(ImageEncodeError::SizeOverflow)?;
    }

    let mut data_cursor = path_cursor;
    let mut data_layout: Vec<DataWindow> = Vec::with_capacity(sorted.len());
    // Одинаковое содержимое кладётся в образ один раз: повторившая его запись
    // ссылается на запись, открывшую окно.
    let mut windows: BTreeMap<&[u8], u16> = BTreeMap::new();
    let mut placed: Vec<(u64, &[u8])> = Vec::new();
    for (index, record) in sorted.iter().enumerate() {
        let data_size =
            u64::try_from(record.data.len()).map_err(|_| ImageEncodeError::SizeOverflow)?;
        if let Some(&owner) = windows.get(record.data) {
            data_layout.push(DataWindow {
                offset: data_layout[usize::from(owner)].offset,
                size: data_size,
                alias_of: owner + 1,
            });
            continue;
        }
        let data_offset = align_up_to_page(data_cursor).ok_or(ImageEncodeError::SizeOverflow)?;
        data_cursor = data_offset
            .checked_add(data_size)
            .ok_or(ImageEncodeError::SizeOverflow)?;
        let index = u16::try_from(index).map_err(|_| ImageEncodeError::TooManyRecords)?;
        windows.insert(record.data, index);
        placed.push((data_offset, record.data));
        data_layout.push(DataWindow {
            offset: data_offset,
            size: data_size,
            alias_of: 0,
        });
    }
    let total_size = usize::try_from(data_cursor).map_err(|_| ImageEncodeError::SizeOverflow)?;

    let mut image = Vec::with_capacity(total_size);
    image.extend_from_slice(&USERLAND_IMAGE_MAGIC);
    image.extend_from_slice(&USERLAND_IMAGE_VERSION.to_le_bytes());
    image.extend_from_slice(&entry_count.to_le_bytes());
    image.extend_from_slice(&boot_index.to_le_bytes());
    image.extend_from_slice(&0u16.to_le_bytes());
    image.extend_from_slice(&data_cursor.to_le_bytes());

    for (&(path_offset, path_len), window) in table.iter().zip(&data_layout) {
        image.extend_from_slice(&path_offset.to_le_bytes());
        image.extend_from_slice(&path_len.to_le_bytes());
        image.extend_from_slice(&window.alias_of.to_le_bytes());
        image.extend_from_slice(&window.offset.to_le_bytes());
        image.extend_from_slice(&window.size.to_le_bytes());
    }

    image.extend_from_slice(&paths);
    for (data_offset, data) in placed {
        let data_offset =
            usize::try_from(data_offset).map_err(|_| ImageEncodeError::SizeOverflow)?;
        image.resize(data_offset, 0);
        image.extend_from_slice(data);
    }
    Ok(image)
}

/// Раскладывает программу в содержимое записи: заголовок и дескрипторы
/// сегментов на первой странице, байты сегментов - за ней.
pub fn encode_program<'a, E: EntryView<'a>>(entry: &E) -> Result<Vec<u8>, ImageEncodeError> {
    validate_entry(entry).map_err(ImageEncodeError::Semantic)?;
    let segment_count =
        u16::try_from(entry.segments().count()).map_err(|_| ImageEncodeError::TooManySegments)?;
    let descriptors_end = usize::from(segment_count)
        .checked_mul(SEGMENT_SIZE)
        .and_then(|size| size.checked_add(PROGRAM_HEADER_SIZE))
        .ok_or(ImageEncodeError::SizeOverflow)?;
    if descriptors_end > USERLAND_PAGE_SIZE as usize {
        return Err(ImageEncodeError::TooManySegments);
    }

    let mut descriptors = Vec::with_capacity(usize::from(segment_count) * SEGMENT_SIZE);
    let mut segments = Vec::new();
    let mut cursor = USERLAND_PAGE_SIZE;

    for segment in entry.segments() {
        let file_offset = align_up_to_page(cursor).ok_or(ImageEncodeError::SizeOverflow)?;
        let file_size =
            u64::try_from(segment.bytes().len()).map_err(|_| ImageEncodeError::SizeOverflow)?;
        cursor = file_offset
            .checked_add(file_size)
            .ok_or(ImageEncodeError::SizeOverflow)?;

        descriptors.extend_from_slice(&file_offset.to_le_bytes());
        descriptors.extend_from_slice(&file_size.to_le_bytes());
        descriptors.extend_from_slice(&segment.va_base().to_le_bytes());
        descriptors.extend_from_slice(&segment.mem_size().to_le_bytes());
        descriptors.extend_from_slice(&encode_permissions(segment.permissions()).to_le_bytes());
        descriptors.extend_from_slice(&0u32.to_le_bytes());
        segments.push((file_offset, segment.bytes()));
    }

    let mut program = Vec::new();
    program.extend_from_slice(&USERLAND_PROGRAM_MAGIC);
    program.extend_from_slice(&entry.entry_va().to_le_bytes());
    program.extend_from_slice(&entry.stack_size().to_le_bytes());
    program.extend_from_slice(&segment_count.to_le_bytes());
    program.extend_from_slice(&[0u8; 6]);
    program.extend_from_slice(&descriptors);

    for (file_offset, bytes) in segments {
        let file_offset =
            usize::try_from(file_offset).map_err(|_| ImageEncodeError::SizeOverflow)?;
        program.resize(file_offset, 0);
        program.extend_from_slice(bytes);
    }
    Ok(program)
}

fn align_up_to_page(offset: u64) -> Option<u64> {
    offset.checked_next_multiple_of(USERLAND_PAGE_SIZE)
}

const fn encode_permissions(permissions: SegmentPermissions) -> u32 {
    match permissions {
        SegmentPermissions::ReadWrite => 0,
        SegmentPermissions::ReadOnly => 1,
        SegmentPermissions::ReadExecute => 2,
    }
}
