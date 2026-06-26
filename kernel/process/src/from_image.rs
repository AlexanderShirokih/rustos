//! Преобразование семантической модели userland в исполняемый образ ядра.

use alloc::vec::Vec;

use memory::{
    MemFlags, PAGE_SIZE,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use userland::{
    EntryView, SegmentPermissions, SegmentView, UserlandValidationError, validate_entry,
};

use crate::image::{UserImage, UserSegment};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserImageFromModelError {
    Semantic(UserlandValidationError),
    SegmentSizeOverflow,
    FieldOverflow,
}

pub struct UserImageParts<'a> {
    segments: Vec<UserSegment<'a>>,
    entry: VirtualAddress,
    user_stack_top: VirtualAddress,
    user_stack_size: usize,
}

impl UserImageParts<'_> {
    pub fn image(&self) -> UserImage<'_> {
        UserImage {
            segments: &self.segments,
            entry: self.entry,
            user_stack_top: self.user_stack_top,
            user_stack_size: self.user_stack_size,
        }
    }
}

pub fn user_image_parts_from_entry<'a, E: EntryView<'a>>(
    entry: &E,
    user_va_end: usize,
) -> Result<UserImageParts<'a>, UserImageFromModelError> {
    validate_entry(entry).map_err(UserImageFromModelError::Semantic)?;

    let mut segments = Vec::new();
    for segment in entry.segments() {
        let va_base = usize::try_from(segment.va_base())
            .map_err(|_| UserImageFromModelError::FieldOverflow)?;
        let va_base = PageAlignedVirtualAddress::from_usize(va_base).ok_or(
            UserImageFromModelError::Semantic(UserlandValidationError::MisalignedSegment),
        )?;
        let mem_size = usize::try_from(segment.mem_size())
            .map_err(|_| UserImageFromModelError::SegmentSizeOverflow)?;
        let mapped_size =
            round_up_to_frame(mem_size).ok_or(UserImageFromModelError::SegmentSizeOverflow)?;

        segments.push(UserSegment {
            va_base,
            mapped_size,
            init_bytes: segment.bytes(),
            perms: permissions_to_mem_flags(segment.permissions()),
        });
    }

    let entry_va =
        usize::try_from(entry.entry_va()).map_err(|_| UserImageFromModelError::FieldOverflow)?;
    let user_stack_size =
        usize::try_from(entry.stack_size()).map_err(|_| UserImageFromModelError::FieldOverflow)?;

    Ok(UserImageParts {
        segments,
        entry: VirtualAddress::new(entry_va),
        user_stack_top: VirtualAddress::new(user_va_end),
        user_stack_size,
    })
}

fn permissions_to_mem_flags(permissions: SegmentPermissions) -> MemFlags {
    match permissions {
        SegmentPermissions::ReadWrite => MemFlags::user_rw(),
        SegmentPermissions::ReadOnly => MemFlags::user_ro(),
        SegmentPermissions::ReadExecute => MemFlags::user_rx(),
    }
}

fn round_up_to_frame(value: usize) -> Option<usize> {
    let rem = value % PAGE_SIZE;
    if rem == 0 {
        return Some(value);
    }
    value.checked_add(PAGE_SIZE.get() - rem)
}

#[cfg(test)]
mod tests {
    use memory::mem_flags::{AccessMode, Executable};
    use userland::{Entry, Segment, SegmentPermissions};

    use super::*;

    const PAGE: usize = PAGE_SIZE.get();
    const TEST_USER_VA_END: usize = 0x1_0000_0000;

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

    #[test]
    fn converts_semantic_entry_without_wire_format() {
        let segments = [
            Segment {
                va_base: 0x40_0000,
                mem_size: PAGE as u64,
                permissions: SegmentPermissions::ReadExecute,
                bytes: b"CODE",
            },
            Segment {
                va_base: 0x40_1000,
                mem_size: 0x500,
                permissions: SegmentPermissions::ReadWrite,
                bytes: b"DATA",
            },
        ];
        let entry = Entry {
            name: "test",
            entry_va: 0x40_0000,
            stack_size: 0x8000,
            segments: &segments,
        };

        let parts =
            user_image_parts_from_entry(&entry, TEST_USER_VA_END).expect("convert semantic entry");
        let image = parts.image();

        assert_eq!(image.segments[0].init_bytes, b"CODE");
        assert_eq!(image.segments[1].init_bytes, b"DATA");
        assert_eq!(image.segments[1].mapped_size, PAGE);
        assert_eq!(user_perms(image.segments[0].perms), (true, false, true));
        assert_eq!(user_perms(image.segments[1].perms), (true, true, false));
        assert_eq!(image.user_stack_top, VirtualAddress::new(TEST_USER_VA_END));
        assert_eq!(image.user_stack_size, 0x8000);
        assert_eq!(image.validate(), Ok(()));
    }

    #[test]
    fn reports_semantic_validation_errors() {
        let segments = [Segment {
            va_base: 0x40_0001,
            mem_size: PAGE as u64,
            permissions: SegmentPermissions::ReadExecute,
            bytes: b"CODE",
        }];
        let entry = Entry {
            name: "test",
            entry_va: 0x40_0001,
            stack_size: 0x4000,
            segments: &segments,
        };

        assert_eq!(
            user_image_parts_from_entry(&entry, TEST_USER_VA_END).map(|_| ()),
            Err(UserImageFromModelError::Semantic(
                UserlandValidationError::MisalignedSegment
            ))
        );
    }

    #[test]
    fn reports_segment_size_overflow() {
        // u64::MAX проходит validate_entry (u64-арифметика), но переполняет
        // usize в round_up_to_frame -> SegmentSizeOverflow без паники.
        let segments = [Segment {
            va_base: 0,
            mem_size: u64::MAX,
            permissions: SegmentPermissions::ReadExecute,
            bytes: b"CODE",
        }];
        let entry = Entry {
            name: "test",
            entry_va: 0,
            stack_size: 0x4000,
            segments: &segments,
        };

        assert_eq!(
            user_image_parts_from_entry(&entry, TEST_USER_VA_END).map(|_| ()),
            Err(UserImageFromModelError::SegmentSizeOverflow)
        );
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
