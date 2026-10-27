use component_abi::{COMPONENT_MANIFEST_SECTION, encode_manifest, manifest_len};

use crate::{
    ProgramView, Segment, SegmentPermissions, USERLAND_PAGE_SIZE, UserlandValidationError,
    elf::{ElfError, ElfFile, PF_R, PF_W, PF_X, PT_GNU_STACK, PT_LOAD, ProgramHeader},
    validate_program,
};

static EMPTY_MANIFEST: [u8; manifest_len(&[], &[], &[])] = encode_manifest(&[], &[], &[]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramDecodeError {
    /// Буфер не покрывает метаданные программы.
    BufferTooShort { needed: usize, actual: usize },
    /// Раскладка заголовков или сегментов нарушает указанное правило.
    InvalidLayout(&'static str),
    /// Диапазон данных сегмента или секции выходит за пределы программы.
    OutOfBounds { offset: u64, size: u64 },
    /// Модель программы нарушает условия загрузки.
    Semantic(UserlandValidationError),
}

impl From<ElfError> for ProgramDecodeError {
    fn from(error: ElfError) -> Self {
        match error {
            ElfError::TooShort { needed, actual } => Self::BufferTooShort { needed, actual },
            ElfError::OutOfBounds { offset, size } => Self::OutOfBounds { offset, size },
            ElfError::Layout(rule) => Self::InvalidLayout(rule),
        }
    }
}

/// Программа, заимствующая байты ELF, из которых она разобрана.
#[derive(Debug, Clone, Copy)]
pub struct DecodedProgram<'a> {
    elf: ElfFile<'a>,
    stack_size: u64,
    manifest: &'a [u8],
}

impl<'a> ProgramView<'a> for DecodedProgram<'a> {
    type Segment = Segment<'a>;

    fn manifest(&self) -> &'a [u8] {
        self.manifest
    }

    fn entry_va(&self) -> u64 {
        self.elf.entry()
    }

    fn stack_size(&self) -> u64 {
        self.stack_size
    }

    fn segments(&self) -> impl Iterator<Item = Self::Segment> + '_ {
        // Программа читается из чужой памяти, и неполная запись обрывает перечисление вместо
        // паники.
        let elf = self.elf;
        elf.program_headers()
            .filter(|header| header.kind == PT_LOAD)
            .map_while(move |header| {
                Some(Segment {
                    va_base: header.va_base,
                    mem_size: header.mem_size,
                    permissions: decode_permissions(header.flags)?,
                    bytes: elf.bytes(header.offset, header.file_size)?,
                })
            })
    }
}

pub fn decode_program(data: &[u8]) -> Result<DecodedProgram<'_>, ProgramDecodeError> {
    let elf = ElfFile::parse(data)?;

    let mut stack_size = None;
    for header in elf.program_headers() {
        match header.kind {
            PT_LOAD => validate_load(header, elf.len())?,
            PT_GNU_STACK => {
                // Первый `PT_GNU_STACK` задаёт стек, остальные игнорируются.
                stack_size.get_or_insert(header.mem_size);
            }
            _ => {}
        }
    }
    let stack_size = stack_size.ok_or(ProgramDecodeError::InvalidLayout("stack_header"))?;

    let manifest = match elf.section_range(COMPONENT_MANIFEST_SECTION)? {
        Some(range) => {
            // Секция в загружаемом сегменте попала бы в память процесса.
            let inside_segment = elf
                .program_headers()
                .filter(|header| header.kind == PT_LOAD)
                .any(|header| overlaps(&range, header));
            if inside_segment {
                return Err(ProgramDecodeError::InvalidLayout("manifest_in_segment"));
            }
            elf.slice(range)
                .ok_or(ProgramDecodeError::InvalidLayout("manifest_section"))?
        }
        None => EMPTY_MANIFEST.as_slice(),
    };

    let program = DecodedProgram {
        elf,
        stack_size,
        manifest,
    };
    validate_program(&program).map_err(ProgramDecodeError::Semantic)?;

    Ok(program)
}

fn validate_load(header: ProgramHeader, data_len: usize) -> Result<(), ProgramDecodeError> {
    if decode_permissions(header.flags).is_none() {
        return Err(ProgramDecodeError::InvalidLayout("segment_flags"));
    }
    let out_of_bounds = ProgramDecodeError::OutOfBounds {
        offset: header.offset,
        size: header.file_size,
    };
    // Сегмент без байтов в образе не отображается срезом записи, и его смещение ничего не
    // задаёт.
    if header.file_size == 0 {
        return Ok(());
    }
    if !header.offset.is_multiple_of(USERLAND_PAGE_SIZE) {
        return Err(ProgramDecodeError::InvalidLayout("segment_alignment"));
    }
    let end = header
        .offset
        .checked_add(header.file_size)
        .ok_or(out_of_bounds)?;
    if u64::try_from(data_len).is_ok_and(|len| end > len) {
        return Err(out_of_bounds);
    }

    Ok(())
}

fn overlaps(range: &core::ops::Range<usize>, header: ProgramHeader) -> bool {
    let (Ok(start), Ok(size)) = (
        usize::try_from(header.offset),
        usize::try_from(header.file_size),
    ) else {
        return true;
    };
    let end = start.saturating_add(size);
    !range.is_empty() && start < end && start < range.end && range.start < end
}

fn decode_permissions(flags: u32) -> Option<SegmentPermissions> {
    match flags & (PF_R | PF_W | PF_X) {
        value if value == PF_R | PF_W => Some(SegmentPermissions::ReadWrite),
        PF_R => Some(SegmentPermissions::ReadOnly),
        value if value == PF_R | PF_X => Some(SegmentPermissions::ReadExecute),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Program, Segment,
        elf::{
            ELF_HEADER_SIZE, ERROR_MACHINE, FILE_MACHINE, PROGRAM_FILE_SIZE, PROGRAM_HEADER_SIZE,
            PROGRAM_KIND, PROGRAM_MEM_SIZE, PROGRAM_OFFSET, PROGRAM_VA_BASE, SECTION_OFFSET,
            section_header_at,
        },
        encode_elf_program,
    };

    const MANIFEST: &[u8] = b"MANIFEST";
    const PAGE: usize = syscall::PAGE_SIZE;

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
        encode_elf_program(&Program {
            manifest: MANIFEST,
            entry_va: 0x4000_0000,
            stack_size: 0x4000,
            segments: &segments,
        })
        .expect("encode program")
    }

    fn put(bytes: &mut [u8], offset: usize, value: &[u8]) {
        bytes[offset..offset + value.len()].copy_from_slice(value);
    }

    fn header_at(index: usize) -> usize {
        ELF_HEADER_SIZE + index * PROGRAM_HEADER_SIZE
    }

    #[test]
    fn round_trip_gives_the_program_model() {
        let bytes = program_bytes();
        let program = decode_program(&bytes).expect("decode program");
        let segments: Vec<_> = program.segments().collect();

        assert_eq!(program.entry_va(), 0x4000_0000);
        assert_eq!(program.stack_size(), 0x4000);
        assert_eq!(program.manifest(), MANIFEST);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].bytes, b"CODE");
        assert_eq!(segments[0].permissions, SegmentPermissions::ReadExecute);
        assert_eq!(segments[1].bytes, b"RW");
        assert_eq!(segments[1].permissions, SegmentPermissions::ReadWrite);
    }

    #[test]
    fn decode_reads_a_program_from_bytes_with_page_slack_past_its_end() {
        let mut bytes = program_bytes();
        bytes.resize(bytes.len() + PAGE, 0);

        let program = decode_program(&bytes).expect("slack past the program is not a segment");
        let segments: Vec<_> = program.segments().collect();
        assert_eq!(segments[0].bytes, b"CODE");
        assert_eq!(segments[1].bytes, b"RW");
    }

    #[test]
    fn elf_failures_surface_as_layout_failures() {
        let mut bytes = program_bytes();
        put(&mut bytes, FILE_MACHINE, &243u16.to_le_bytes());

        assert_eq!(
            decode_program(&bytes).expect_err("foreign architecture must fail"),
            ProgramDecodeError::InvalidLayout(ERROR_MACHINE)
        );
    }

    #[test]
    fn rejects_segment_reaching_past_the_program() {
        let mut bytes = program_bytes();
        put(
            &mut bytes,
            header_at(0) + PROGRAM_FILE_SIZE,
            &u64::MAX.to_le_bytes(),
        );

        assert!(matches!(
            decode_program(&bytes),
            Err(ProgramDecodeError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn empty_segment_stays_visible_whatever_its_offset() {
        let segments = [
            segment(0x4000_0000, SegmentPermissions::ReadExecute, b"CODE"),
            segment(0x4000_1000, SegmentPermissions::ReadWrite, b""),
            segment(0x4000_2000, SegmentPermissions::ReadWrite, b"RW"),
        ];
        let mut bytes = encode_elf_program(&Program {
            manifest: MANIFEST,
            entry_va: 0x4000_0000,
            stack_size: 0x4000,
            segments: &segments,
        })
        .expect("encode program");
        put(
            &mut bytes,
            header_at(1) + PROGRAM_OFFSET,
            &u64::MAX.to_le_bytes(),
        );

        let program = decode_program(&bytes).expect("empty segment ignores its offset");
        let loaded: Vec<_> = program.segments().map(|segment| segment.bytes).collect();
        assert_eq!(loaded, [b"CODE".as_slice(), b"", b"RW"]);
    }

    #[test]
    fn rejects_unaligned_segment_offset() {
        let mut bytes = program_bytes();
        put(
            &mut bytes,
            header_at(0) + PROGRAM_OFFSET,
            &(PAGE as u64 + 1).to_le_bytes(),
        );

        assert_eq!(
            decode_program(&bytes).expect_err("unaligned segment must fail"),
            ProgramDecodeError::InvalidLayout("segment_alignment")
        );
    }

    #[test]
    fn rejects_semantically_invalid_segments_and_stack() {
        let mut overlap = program_bytes();
        put(
            &mut overlap,
            header_at(1) + PROGRAM_VA_BASE,
            &0x4000_0000u64.to_le_bytes(),
        );
        assert_eq!(
            decode_program(&overlap).expect_err("overlap must fail"),
            ProgramDecodeError::Semantic(UserlandValidationError::OverlappingSegments)
        );

        let mut init_exceeds_memory = program_bytes();
        put(
            &mut init_exceeds_memory,
            header_at(0) + PROGRAM_MEM_SIZE,
            &2u64.to_le_bytes(),
        );
        assert_eq!(
            decode_program(&init_exceeds_memory).expect_err("filesz above memsz must fail"),
            ProgramDecodeError::Semantic(UserlandValidationError::InitBytesExceedMemory)
        );

        let mut zero_stack = program_bytes();
        put(
            &mut zero_stack,
            header_at(2) + PROGRAM_MEM_SIZE,
            &0u64.to_le_bytes(),
        );
        assert_eq!(
            decode_program(&zero_stack).expect_err("zero stack must fail"),
            ProgramDecodeError::Semantic(UserlandValidationError::ZeroStackSize)
        );

        let mut no_stack = program_bytes();
        put(
            &mut no_stack,
            header_at(2) + PROGRAM_KIND,
            &0u32.to_le_bytes(),
        );
        assert_eq!(
            decode_program(&no_stack).expect_err("missing PT_GNU_STACK must fail"),
            ProgramDecodeError::InvalidLayout("stack_header")
        );
    }

    #[test]
    fn rejects_manifest_inside_a_segment() {
        let mut bytes = program_bytes();
        let section = section_header_at(&bytes, 1);
        put(
            &mut bytes,
            section + SECTION_OFFSET,
            &(PAGE as u64).to_le_bytes(),
        );

        assert_eq!(
            decode_program(&bytes).expect_err("manifest inside a segment must fail"),
            ProgramDecodeError::InvalidLayout("manifest_in_segment")
        );
    }

    #[test]
    fn program_without_a_manifest_section_declares_nothing() {
        let mut bytes = program_bytes();
        let name = COMPONENT_MANIFEST_SECTION.as_bytes();
        let at = bytes
            .windows(name.len())
            .position(|window| window == name)
            .expect("section name");
        bytes[at] = b'_';

        let program = decode_program(&bytes).expect("section is optional");
        let manifest = component_abi::decode_manifest(program.manifest()).expect("empty record");
        assert_eq!(manifest.requirements().count(), 0);
        assert_eq!(manifest.exports().count(), 0);
    }
}
