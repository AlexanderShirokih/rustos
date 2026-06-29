#![cfg_attr(not(test), no_std)]

pub const USERLAND_ENTRY_NAME_CAPACITY: usize = 64;
pub const USERLAND_PAGE_SIZE: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentPermissions {
    ReadWrite,
    ReadOnly,
    ReadExecute,
}

impl SegmentPermissions {
    pub const fn is_executable(self) -> bool {
        matches!(self, Self::ReadExecute)
    }
}

pub trait SegmentView<'a>: Copy {
    fn va_base(self) -> u64;
    fn mem_size(self) -> u64;
    fn permissions(self) -> SegmentPermissions;
    fn bytes(self) -> &'a [u8];
}

pub trait EntryView<'a> {
    type Segment: SegmentView<'a>;

    fn name(&self) -> &'a str;
    fn entry_va(&self) -> u64;
    fn stack_size(&self) -> u64;
    fn segments(&self) -> impl Iterator<Item = Self::Segment> + '_;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment<'a> {
    pub va_base: u64,
    pub mem_size: u64,
    pub permissions: SegmentPermissions,
    pub bytes: &'a [u8],
}

impl<'a> SegmentView<'a> for Segment<'a> {
    fn va_base(self) -> u64 {
        self.va_base
    }

    fn mem_size(self) -> u64 {
        self.mem_size
    }

    fn permissions(self) -> SegmentPermissions {
        self.permissions
    }

    fn bytes(self) -> &'a [u8] {
        self.bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub entry_va: u64,
    pub stack_size: u64,
    pub segments: &'a [Segment<'a>],
}

impl<'a> EntryView<'a> for Entry<'a> {
    type Segment = Segment<'a>;

    fn name(&self) -> &'a str {
        self.name
    }

    fn entry_va(&self) -> u64 {
        self.entry_va
    }

    fn stack_size(&self) -> u64 {
        self.stack_size
    }

    fn segments(&self) -> impl Iterator<Item = Self::Segment> + '_ {
        self.segments.iter().copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserlandValidationError {
    EmptyName,
    NonAsciiName,
    NameTooLong,
    NoSegments,
    ZeroStackSize,
    MisalignedSegment,
    ZeroSegmentMemory,
    InitBytesExceedMemory,
    SegmentRangeOverflow,
    OverlappingSegments,
    EntryNotExecutable,
}

pub fn validate_entry<'a, E: EntryView<'a>>(entry: &E) -> Result<(), UserlandValidationError> {
    let name = entry.name();
    if name.is_empty() {
        return Err(UserlandValidationError::EmptyName);
    }
    if !name.is_ascii() {
        return Err(UserlandValidationError::NonAsciiName);
    }
    if name.len() > USERLAND_ENTRY_NAME_CAPACITY {
        return Err(UserlandValidationError::NameTooLong);
    }
    if entry.stack_size() == 0 {
        return Err(UserlandValidationError::ZeroStackSize);
    }

    let mut segment_count = 0;
    let mut entry_is_executable = false;
    for (index, segment) in entry.segments().enumerate() {
        segment_count += 1;
        if !segment.va_base().is_multiple_of(USERLAND_PAGE_SIZE) {
            return Err(UserlandValidationError::MisalignedSegment);
        }
        if segment.mem_size() == 0 {
            return Err(UserlandValidationError::ZeroSegmentMemory);
        }
        if segment.mem_size() < segment.bytes().len() as u64 {
            return Err(UserlandValidationError::InitBytesExceedMemory);
        }
        let segment_end = segment
            .va_base()
            .checked_add(segment.mem_size())
            .ok_or(UserlandValidationError::SegmentRangeOverflow)?;

        for other in entry.segments().take(index) {
            let other_end = other
                .va_base()
                .checked_add(other.mem_size())
                .ok_or(UserlandValidationError::SegmentRangeOverflow)?;
            if segment_end > other.va_base() && other_end > segment.va_base() {
                return Err(UserlandValidationError::OverlappingSegments);
            }
        }

        entry_is_executable |= segment.permissions().is_executable()
            && entry.entry_va() >= segment.va_base()
            && entry.entry_va() < segment_end;
    }

    if segment_count == 0 {
        return Err(UserlandValidationError::NoSegments);
    }
    if !entry_is_executable {
        return Err(UserlandValidationError::EntryNotExecutable);
    }
    Ok(())
}

/// Промежуток `[start, end)` между концом старшего сегмента и низом стека -
/// окно, куда раскладываются анонимные маппинги процесса (куча).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserVmWindow {
    pub start: u64,
    pub end: u64,
}

/// Выводит [`UserVmWindow`] из геометрии образа. Конец сегмента округляется
/// вверх до страницы (переполняющий сегмент отбрасывается); низ стека -
/// `stack_top - stack_size`. `None`, если промежутка нет либо стек переполняет.
pub fn user_vm_window(
    segments: impl Iterator<Item = (u64, u64)>,
    stack_top: u64,
    stack_size: u64,
) -> Option<UserVmWindow> {
    let highest_end = segments
        .filter_map(|(va_base, mem_size)| {
            va_base
                .checked_add(mem_size)?
                .checked_next_multiple_of(USERLAND_PAGE_SIZE)
        })
        .max()?;
    let stack_base = stack_top.checked_sub(stack_size)?;
    if highest_end >= stack_base {
        return None;
    }
    Some(UserVmWindow {
        start: highest_end,
        end: stack_base,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: u64 = USERLAND_PAGE_SIZE;

    fn segment(va_base: u64, mem_size: u64, permissions: SegmentPermissions) -> Segment<'static> {
        Segment {
            va_base,
            mem_size,
            permissions,
            bytes: b"DATA",
        }
    }

    fn entry<'a>(name: &'a str, segments: &'a [Segment<'a>], entry_va: u64) -> Entry<'a> {
        Entry {
            name,
            entry_va,
            stack_size: PAGE,
            segments,
        }
    }

    #[test]
    fn valid_entry_passes_validation() {
        let segments = [
            segment(0x4000_0000, PAGE, SegmentPermissions::ReadExecute),
            segment(0x4000_1000, PAGE, SegmentPermissions::ReadWrite),
        ];
        assert_eq!(
            validate_entry(&entry("rootkeeper", &segments, 0x4000_0000)),
            Ok(())
        );
    }

    #[test]
    fn rejects_invalid_names() {
        let segments = [segment(0x4000_0000, PAGE, SegmentPermissions::ReadExecute)];
        assert_eq!(
            validate_entry(&entry("", &segments, 0x4000_0000)),
            Err(UserlandValidationError::EmptyName)
        );
        assert_eq!(
            validate_entry(&entry("не ascii", &segments, 0x4000_0000)),
            Err(UserlandValidationError::NonAsciiName)
        );

        let long_name = "x".repeat(USERLAND_ENTRY_NAME_CAPACITY + 1);
        assert_eq!(
            validate_entry(&entry(&long_name, &segments, 0x4000_0000)),
            Err(UserlandValidationError::NameTooLong)
        );
    }

    #[test]
    fn rejects_empty_segments_and_zero_stack() {
        let empty = entry("rootkeeper", &[], 0x4000_0000);
        assert_eq!(
            validate_entry(&empty),
            Err(UserlandValidationError::NoSegments)
        );

        let segments = [segment(0x4000_0000, PAGE, SegmentPermissions::ReadExecute)];
        let mut zero_stack = entry("rootkeeper", &segments, 0x4000_0000);
        zero_stack.stack_size = 0;
        assert_eq!(
            validate_entry(&zero_stack),
            Err(UserlandValidationError::ZeroStackSize)
        );
    }

    #[test]
    fn rejects_invalid_segment_memory() {
        let zero = [segment(0x4000_0000, 0, SegmentPermissions::ReadExecute)];
        assert_eq!(
            validate_entry(&entry("rootkeeper", &zero, 0x4000_0000)),
            Err(UserlandValidationError::ZeroSegmentMemory)
        );

        let too_small = [segment(0x4000_0000, 2, SegmentPermissions::ReadExecute)];
        assert_eq!(
            validate_entry(&entry("rootkeeper", &too_small, 0x4000_0000)),
            Err(UserlandValidationError::InitBytesExceedMemory)
        );
    }

    #[test]
    fn rejects_misaligned_overflowing_and_overlapping_segments() {
        let misaligned = [segment(0x4000_0001, PAGE, SegmentPermissions::ReadExecute)];
        assert_eq!(
            validate_entry(&entry("rootkeeper", &misaligned, 0x4000_0001)),
            Err(UserlandValidationError::MisalignedSegment)
        );

        let overflow = [segment(
            u64::MAX - (PAGE - 1),
            PAGE,
            SegmentPermissions::ReadExecute,
        )];
        assert_eq!(
            validate_entry(&entry("rootkeeper", &overflow, u64::MAX)),
            Err(UserlandValidationError::SegmentRangeOverflow)
        );

        let overlap = [
            segment(0x4000_0000, 2 * PAGE, SegmentPermissions::ReadExecute),
            segment(0x4000_1000, PAGE, SegmentPermissions::ReadWrite),
        ];
        assert_eq!(
            validate_entry(&entry("rootkeeper", &overlap, 0x4000_0000)),
            Err(UserlandValidationError::OverlappingSegments)
        );
    }

    #[test]
    fn rejects_entry_outside_executable_segment() {
        let segments = [segment(0x4000_0000, PAGE, SegmentPermissions::ReadWrite)];
        assert_eq!(
            validate_entry(&entry("rootkeeper", &segments, 0x4000_0000)),
            Err(UserlandValidationError::EntryNotExecutable)
        );
    }

    #[test]
    fn user_vm_window_spans_highest_segment_to_stack_base() {
        // Старший сегмент кончается на 0x40_2000 (0x1800 округлён вверх до 2
        // страниц); стек top 0x1_0000_0000 size 0x4000 -> base ниже на 0x4000.
        let window = user_vm_window(
            [(0x40_0000, PAGE), (0x40_1000, 0x1800)].into_iter(),
            0x1_0000_0000,
            0x4000,
        )
        .expect("gap exists");
        assert_eq!(window.start, 0x40_3000);
        assert_eq!(window.end, 0x1_0000_0000 - 0x4000);
    }

    #[test]
    fn user_vm_window_none_without_gap() {
        assert_eq!(
            user_vm_window([(0, 0x10_0000)].into_iter(), 0x10_0000, PAGE),
            None
        );
    }

    #[test]
    fn user_vm_window_skips_overflowing_segment() {
        // Переполняющий сегмент отброшен, границу задаёт валидный.
        let window = user_vm_window(
            [(0x40_0000, PAGE), (u64::MAX, PAGE)].into_iter(),
            0x1_0000_0000,
            PAGE,
        )
        .expect("valid segment bounds window");
        assert_eq!(window.start, 0x40_1000);
    }

    #[test]
    fn user_vm_window_none_without_segments() {
        assert_eq!(
            user_vm_window(core::iter::empty(), 0x1_0000_0000, PAGE),
            None
        );
    }
}
