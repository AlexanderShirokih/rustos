//! Мост из распарсенного [`UserlandImageEntry`] в [`UserImage`].
//!
//! Бридж самодостаточно валидирует входные данные (выравнивание, флаги,
//! границы payload-среза) и не паникует на кривом образе. Итоговую проверку
//! инвариантов (пересечения, entry в исполняемом сегменте) выполняет
//! [`UserImage::validate`].

use alloc::vec::Vec;

use memory::{
    MemFlags,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use userland_abi::{SegmentPermissions, UserlandImageEntry};

use crate::image::{UserImage, UserSegment};

const FRAME_SIZE: usize = 4096;

/// Ошибки конвертации [`UserlandImageEntry`] в [`UserImage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserImageFromAbiError {
    /// Базовый VA сегмента не выровнен на 4К.
    MisalignedSegmentBase(u64),
    /// Значение флагов сегмента не соответствует ни одной известной комбинации
    /// прав (ожидается 0/1/2).
    UnknownSegmentFlags(u32),
    /// Срез init-байт сегмента выходит за границы payload или вычисление его
    /// смещения/длины переполняется.
    InitBytesOutOfBounds,
    /// `mapped_size` сегмента, округлённый вверх до 4К, переполняет `usize`.
    SegmentSizeOverflow,
    /// Поле `stack_size` или `entry_va` не помещается в `usize`.
    FieldOverflow,
}

/// Сегменты user-образа, собранные из [`UserlandImageEntry`]. Владеет `Vec`,
/// чтобы [`UserImage`] мог заимствовать срез; init-байты по-прежнему ссылаются
/// на payload исходного blob-а (lifetime `'a`).
pub struct UserImageParts<'a> {
    segments: Vec<UserSegment<'a>>,
    entry: VirtualAddress,
    user_stack_top: VirtualAddress,
    user_stack_size: usize,
}

impl UserImageParts<'_> {
    /// Собирает заимствующий [`UserImage`]. Полученный образ ещё не
    /// провалидирован - вызови [`UserImage::validate`] перед загрузкой.
    pub fn image(&self) -> UserImage<'_> {
        UserImage {
            segments: &self.segments,
            entry: self.entry,
            user_stack_top: self.user_stack_top,
            user_stack_size: self.user_stack_size,
        }
    }
}

/// Конвертирует один [`UserlandImageEntry`] в [`UserImageParts`].
///
/// Стек размещается по политике "верх user-диапазона": вершина - `user_va_end`
/// (потолок user-VA арха), размер - `stack_size` из заголовка (ABI гарантирует
/// != 0). Образ обязан лежать ниже `user_va_end - stack_size`.
pub fn user_image_parts_from_entry<'a>(
    entry: &UserlandImageEntry<'a>,
    user_va_end: usize,
) -> Result<UserImageParts<'a>, UserImageFromAbiError> {
    let header = entry.header();
    let payload = entry.payload();

    let mut segments = Vec::with_capacity(entry.segment_count());
    for segment in entry.segments() {
        let perms = perms_from_flags(segment.flags)?;

        let va_base_usize =
            usize::try_from(segment.va_base).map_err(|_| UserImageFromAbiError::FieldOverflow)?;
        let va_base = PageAlignedVirtualAddress::from_usize(va_base_usize).ok_or(
            UserImageFromAbiError::MisalignedSegmentBase(segment.va_base),
        )?;

        let mem_size = usize::try_from(segment.mem_size)
            .map_err(|_| UserImageFromAbiError::SegmentSizeOverflow)?;
        let mapped_size =
            round_up_to_frame(mem_size).ok_or(UserImageFromAbiError::SegmentSizeOverflow)?;

        // payload берётся относительно entry: file_offset отсчитывается от
        // начала blob-а, а payload-срез - от payload_offset.
        let relative = (segment.file_offset)
            .checked_sub(header.payload_offset)
            .and_then(|rel| usize::try_from(rel).ok())
            .ok_or(UserImageFromAbiError::InitBytesOutOfBounds)?;
        let file_size = usize::try_from(segment.file_size)
            .map_err(|_| UserImageFromAbiError::InitBytesOutOfBounds)?;
        let end = relative
            .checked_add(file_size)
            .ok_or(UserImageFromAbiError::InitBytesOutOfBounds)?;
        let init_bytes = payload
            .get(relative..end)
            .ok_or(UserImageFromAbiError::InitBytesOutOfBounds)?;

        segments.push(UserSegment {
            va_base,
            mapped_size,
            init_bytes,
            perms,
        });
    }

    let entry_va =
        usize::try_from(header.entry_va).map_err(|_| UserImageFromAbiError::FieldOverflow)?;
    let user_stack_size =
        usize::try_from(header.stack_size).map_err(|_| UserImageFromAbiError::FieldOverflow)?;

    Ok(UserImageParts {
        segments,
        entry: VirtualAddress::new(entry_va),
        user_stack_top: VirtualAddress::new(user_va_end),
        user_stack_size,
    })
}

fn perms_from_flags(flags: u32) -> Result<MemFlags, UserImageFromAbiError> {
    match SegmentPermissions::from_flags(flags)
        .ok_or(UserImageFromAbiError::UnknownSegmentFlags(flags))?
    {
        SegmentPermissions::ReadWrite => Ok(MemFlags::user_rw()),
        SegmentPermissions::ReadOnly => Ok(MemFlags::user_ro()),
        SegmentPermissions::ReadExecute => Ok(MemFlags::user_rx()),
    }
}

/// Округляет `value` вверх до кратного 4К. `None` при переполнении `usize`.
fn round_up_to_frame(value: usize) -> Option<usize> {
    let rem = value % FRAME_SIZE;
    if rem == 0 {
        return Some(value);
    }
    value.checked_add(FRAME_SIZE - rem)
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use memory::mem_flags::{AccessMode, Executable};
    use userland_abi::{
        ImageEntryInput as TestEntry, ImageSegmentInput as TestSegment, UserlandImage,
        build_userland_image,
    };

    use super::*;

    const PAGE: usize = FRAME_SIZE;
    const TEST_USER_VA_END: usize = 0x1_0000_0000;

    // MemFlags не реализует PartialEq, поэтому сводим права к наблюдаемой
    // тройке (read, write, exec) user-владельца для сравнения в тестах.
    fn user_perms(flags: MemFlags) -> (bool, bool, bool) {
        match flags {
            MemFlags::Private(owners) => (
                matches!(
                    owners.user.access,
                    AccessMode::Readonly | AccessMode::Writable
                ),
                matches!(owners.user.access, AccessMode::Writable),
                matches!(owners.user.executable, Executable::Allowed),
            ),
            MemFlags::Device(_) => (false, false, false),
        }
    }

    fn build_image(entries: &[TestEntry<'_>]) -> Vec<u8> {
        build_userland_image(entries).expect("test image must serialize")
    }

    #[test]
    fn maps_all_three_flag_kinds() {
        let image_bytes = build_image(&[TestEntry {
            name: "rootkeeper",
            entry_va: 0x40_0000,
            stack_size: 0x4000,
            segments: &[
                TestSegment {
                    va_base: 0x40_0000,
                    mem_size: PAGE as u64,
                    flags: 2,
                    bytes: b"CODE",
                },
                TestSegment {
                    va_base: 0x40_1000,
                    mem_size: PAGE as u64,
                    flags: 1,
                    bytes: b"RO",
                },
                TestSegment {
                    va_base: 0x40_2000,
                    mem_size: PAGE as u64,
                    flags: 0,
                    bytes: b"RW",
                },
            ],
        }]);

        let image = UserlandImage::parse(&image_bytes).expect("parse");
        let parts = user_image_parts_from_entry(&image.bootstrap_entry(), TEST_USER_VA_END)
            .expect("convert");
        let built = parts.image();

        // (read, write, exec) для user-владельца.
        assert_eq!(user_perms(built.segments[0].perms), (true, false, true));
        assert_eq!(user_perms(built.segments[1].perms), (true, false, false));
        assert_eq!(user_perms(built.segments[2].perms), (true, true, false));
    }

    #[test]
    fn rounds_unaligned_mem_size_up_to_frame() {
        // mem_size = 0x500 (не кратен 4К, но >= file_size) -> mapped_size = PAGE.
        let image_bytes = build_image(&[TestEntry {
            name: "rk",
            entry_va: 0x40_0000,
            stack_size: 0x4000,
            segments: &[TestSegment {
                va_base: 0x40_0000,
                mem_size: 0x500,
                flags: 2,
                bytes: b"X",
            }],
        }]);

        let image = UserlandImage::parse(&image_bytes).expect("parse");
        let parts = user_image_parts_from_entry(&image.bootstrap_entry(), TEST_USER_VA_END)
            .expect("convert");
        let built = parts.image();

        assert_eq!(built.segments[0].mapped_size, PAGE);
    }

    #[test]
    fn keeps_already_aligned_mem_size() {
        let image_bytes = build_image(&[TestEntry {
            name: "rk",
            entry_va: 0x40_0000,
            stack_size: 0x4000,
            segments: &[TestSegment {
                va_base: 0x40_0000,
                mem_size: 2 * PAGE as u64,
                flags: 2,
                bytes: b"X",
            }],
        }]);

        let image = UserlandImage::parse(&image_bytes).expect("parse");
        let parts = user_image_parts_from_entry(&image.bootstrap_entry(), TEST_USER_VA_END)
            .expect("convert");
        assert_eq!(parts.image().segments[0].mapped_size, 2 * PAGE);
    }

    #[test]
    fn init_bytes_slice_uses_payload_relative_offset() {
        // Два сегмента в одном payload: проверяем, что init_bytes второго
        // взяты по payload-относительному offset, а не абсолютному.
        let image_bytes = build_image(&[TestEntry {
            name: "rk",
            entry_va: 0x40_0000,
            stack_size: 0x4000,
            segments: &[
                TestSegment {
                    va_base: 0x40_0000,
                    mem_size: PAGE as u64,
                    flags: 2,
                    bytes: b"FIRST",
                },
                TestSegment {
                    va_base: 0x40_1000,
                    mem_size: PAGE as u64,
                    flags: 0,
                    bytes: b"SECOND",
                },
            ],
        }]);

        let image = UserlandImage::parse(&image_bytes).expect("parse");
        let parts = user_image_parts_from_entry(&image.bootstrap_entry(), TEST_USER_VA_END)
            .expect("convert");
        let built = parts.image();

        assert_eq!(built.segments[0].init_bytes, b"FIRST");
        assert_eq!(built.segments[1].init_bytes, b"SECOND");
    }

    #[test]
    fn applies_fixed_stack_policy() {
        let image_bytes = build_image(&[TestEntry {
            name: "rk",
            entry_va: 0x40_0000,
            stack_size: 0x8000,
            segments: &[TestSegment {
                va_base: 0x40_0000,
                mem_size: PAGE as u64,
                flags: 2,
                bytes: b"X",
            }],
        }]);

        let image = UserlandImage::parse(&image_bytes).expect("parse");
        let parts = user_image_parts_from_entry(&image.bootstrap_entry(), TEST_USER_VA_END)
            .expect("convert");
        let built = parts.image();

        assert_eq!(built.user_stack_top, VirtualAddress::new(TEST_USER_VA_END));
        assert_eq!(built.user_stack_size, 0x8000);
    }

    #[test]
    fn converted_image_passes_validate() {
        let image_bytes = build_image(&[TestEntry {
            name: "rootkeeper",
            entry_va: 0x40_0000,
            stack_size: 0x4000,
            segments: &[
                TestSegment {
                    va_base: 0x40_0000,
                    mem_size: PAGE as u64,
                    flags: 2,
                    bytes: b"CODE",
                },
                TestSegment {
                    va_base: 0x40_1000,
                    mem_size: 0x500,
                    flags: 0,
                    bytes: b"DATA",
                },
            ],
        }]);

        let image = UserlandImage::parse(&image_bytes).expect("parse");
        let parts = user_image_parts_from_entry(&image.bootstrap_entry(), TEST_USER_VA_END)
            .expect("convert");
        assert_eq!(parts.image().validate(), Ok(()));
    }

    #[test]
    fn round_up_to_frame_detects_overflow() {
        assert_eq!(round_up_to_frame(usize::MAX), None);
        assert_eq!(round_up_to_frame(0), Some(0));
        assert_eq!(round_up_to_frame(1), Some(PAGE));
        assert_eq!(round_up_to_frame(PAGE), Some(PAGE));
        assert_eq!(round_up_to_frame(PAGE + 1), Some(2 * PAGE));
    }
}
