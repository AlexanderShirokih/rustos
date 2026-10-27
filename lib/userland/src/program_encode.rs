use alloc::vec::Vec;

use component_abi::COMPONENT_MANIFEST_SECTION;

use crate::{
    ProgramView, SegmentPermissions, SegmentView, USERLAND_PAGE_SIZE, UserlandValidationError,
    elf::{PF_R, PF_W, PF_X},
    elf_write::{ElfWriteError, Executable, LoadSegment, write_executable},
    validate_program,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramEncodeError {
    /// Число сегментов не представимо в формате.
    TooManySegments,
    /// Размер или смещение не представимы в формате либо адресном пространстве.
    SizeOverflow,
    /// Модель программы нарушает условия загрузки.
    Semantic(UserlandValidationError),
}

impl From<ElfWriteError> for ProgramEncodeError {
    fn from(error: ElfWriteError) -> Self {
        match error {
            ElfWriteError::TooManyHeaders => Self::TooManySegments,
            ElfWriteError::SizeOverflow => Self::SizeOverflow,
        }
    }
}

const fn encode_permissions(permissions: SegmentPermissions) -> u32 {
    match permissions {
        SegmentPermissions::ReadWrite => PF_R | PF_W,
        SegmentPermissions::ReadOnly => PF_R,
        SegmentPermissions::ReadExecute => PF_R | PF_X,
    }
}

/// Кодирует ELF программы: сегменты на границах страниц, размер стека в `PT_GNU_STACK` и
/// секция манифеста.
pub fn encode_elf_program<'a, E: ProgramView<'a>>(
    program: &E,
) -> Result<Vec<u8>, ProgramEncodeError> {
    validate_program(program).map_err(ProgramEncodeError::Semantic)?;

    let segments: Vec<_> = program
        .segments()
        .map(|segment| LoadSegment {
            flags: encode_permissions(segment.permissions()),
            va_base: segment.va_base(),
            mem_size: segment.mem_size(),
            bytes: segment.bytes(),
        })
        .collect();

    Ok(write_executable(&Executable {
        entry: program.entry_va(),
        stack_size: program.stack_size(),
        segments: &segments,
        segment_align: USERLAND_PAGE_SIZE,
        section_name: COMPONENT_MANIFEST_SECTION,
        section: program.manifest(),
    })?)
}
