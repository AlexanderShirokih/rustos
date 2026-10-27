//! Запись минимального ELF64 для AArch64: сегменты `PT_LOAD`, `PT_GNU_STACK` с размером стека
//! и одна именованная секция.

use alloc::vec::Vec;

use crate::elf::{
    ELF_HEADER_SIZE, ELF_MAGIC, ELFCLASS64, ELFDATA2LSB, EM_AARCH64, ET_EXEC, EV_CURRENT, PF_R,
    PF_W, PROGRAM_HEADER_SIZE, PT_GNU_STACK, PT_LOAD, ProgramHeader, SECTION_HEADER_SIZE,
};

const EI_VERSION_CURRENT: u8 = 1;
const IDENT_PADDING: usize = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ElfWriteError {
    TooManyHeaders,
    SizeOverflow,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LoadSegment<'a> {
    pub(crate) flags: u32,
    pub(crate) va_base: u64,
    pub(crate) mem_size: u64,
    pub(crate) bytes: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Executable<'a> {
    pub(crate) entry: u64,
    pub(crate) stack_size: u64,
    pub(crate) segments: &'a [LoadSegment<'a>],
    pub(crate) segment_align: u64,
    pub(crate) section_name: &'a str,
    pub(crate) section: &'a [u8],
}

const STACK_ALIGN: u64 = 16;
const SECTION_TABLE_ALIGN: u64 = 8;
const SECTION_COUNT: u16 = 3;
const SECTION_NAMES_INDEX: u16 = 2;
const SHT_PROGBITS: u32 = 1;
const SHT_STRTAB: u32 = 3;

pub(crate) fn write_executable(executable: &Executable<'_>) -> Result<Vec<u8>, ElfWriteError> {
    let overflow = || ElfWriteError::SizeOverflow;
    let header_count = executable
        .segments
        .len()
        .checked_add(1)
        .and_then(|count| u16::try_from(count).ok())
        .ok_or(ElfWriteError::TooManyHeaders)?;

    let mut cursor = u64::from(header_count)
        .checked_mul(u64::try_from(PROGRAM_HEADER_SIZE).map_err(|_| overflow())?)
        .and_then(|size| size.checked_add(u64::try_from(ELF_HEADER_SIZE).ok()?))
        .ok_or_else(overflow)?;

    let mut offsets = Vec::with_capacity(executable.segments.len());
    for segment in executable.segments {
        let offset = cursor
            .checked_next_multiple_of(executable.segment_align)
            .ok_or_else(overflow)?;
        let size = u64::try_from(segment.bytes.len()).map_err(|_| overflow())?;
        cursor = offset.checked_add(size).ok_or_else(overflow)?;
        offsets.push(offset);
    }

    // Секция лежит за последним сегментом, поэтому её байты не пересекают `PT_LOAD`.
    let section_offset = cursor;
    let section_size = u64::try_from(executable.section.len()).map_err(|_| overflow())?;

    let mut names = alloc::vec![0u8];
    let section_name = u32::try_from(names.len()).map_err(|_| overflow())?;
    names.extend_from_slice(executable.section_name.as_bytes());
    names.push(0);
    let names_name = u32::try_from(names.len()).map_err(|_| overflow())?;
    names.extend_from_slice(b".shstrtab\0");

    let names_offset = section_offset
        .checked_add(section_size)
        .ok_or_else(overflow)?;
    let names_size = u64::try_from(names.len()).map_err(|_| overflow())?;

    // Таблица секций выровнена по 8 байтам, как требует ELF64.
    let sections_offset = names_offset
        .checked_add(names_size)
        .and_then(|end| end.checked_next_multiple_of(SECTION_TABLE_ALIGN))
        .ok_or_else(overflow)?;

    let mut bytes = Vec::new();
    push_file_header(&mut bytes, executable.entry, sections_offset, header_count);

    for (segment, offset) in executable.segments.iter().zip(&offsets) {
        let file_size = u64::try_from(segment.bytes.len()).map_err(|_| overflow())?;
        push_program_header(
            &mut bytes,
            ProgramHeader {
                kind: PT_LOAD,
                flags: segment.flags,
                offset: *offset,
                va_base: segment.va_base,
                file_size,
                mem_size: segment.mem_size,
            },
            executable.segment_align,
        );
    }

    push_program_header(
        &mut bytes,
        ProgramHeader {
            // Размер стека `PT_GNU_STACK` лежит в `p_memsz`, а в файле у него нет байтов.
            kind: PT_GNU_STACK,
            flags: PF_R | PF_W,
            offset: 0,
            va_base: 0,
            file_size: 0,
            mem_size: executable.stack_size,
        },
        STACK_ALIGN,
    );

    for (segment, offset) in executable.segments.iter().zip(&offsets) {
        bytes.resize(usize::try_from(*offset).map_err(|_| overflow())?, 0);
        bytes.extend_from_slice(segment.bytes);
    }

    bytes.extend_from_slice(executable.section);
    bytes.extend_from_slice(&names);
    bytes.resize(usize::try_from(sections_offset).map_err(|_| overflow())?, 0);

    bytes.extend_from_slice(&[0; SECTION_HEADER_SIZE]);
    push_section_header(
        &mut bytes,
        section_name,
        SHT_PROGBITS,
        section_offset,
        section_size,
    );
    push_section_header(&mut bytes, names_name, SHT_STRTAB, names_offset, names_size);

    Ok(bytes)
}

#[allow(clippy::cast_possible_truncation)]
fn push_file_header(bytes: &mut Vec<u8>, entry: u64, sections_offset: u64, header_count: u16) {
    bytes.extend_from_slice(&ELF_MAGIC);
    bytes.extend_from_slice(&[ELFCLASS64, ELFDATA2LSB, EI_VERSION_CURRENT]);
    bytes.extend_from_slice(&[0; IDENT_PADDING]);
    bytes.extend_from_slice(&ET_EXEC.to_le_bytes());
    bytes.extend_from_slice(&EM_AARCH64.to_le_bytes());
    bytes.extend_from_slice(&EV_CURRENT.to_le_bytes());
    bytes.extend_from_slice(&entry.to_le_bytes());
    bytes.extend_from_slice(&(ELF_HEADER_SIZE as u64).to_le_bytes());
    bytes.extend_from_slice(&sections_offset.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&(ELF_HEADER_SIZE as u16).to_le_bytes());
    bytes.extend_from_slice(&(PROGRAM_HEADER_SIZE as u16).to_le_bytes());
    bytes.extend_from_slice(&header_count.to_le_bytes());
    bytes.extend_from_slice(&(SECTION_HEADER_SIZE as u16).to_le_bytes());
    bytes.extend_from_slice(&SECTION_COUNT.to_le_bytes());
    bytes.extend_from_slice(&SECTION_NAMES_INDEX.to_le_bytes());
}

fn push_program_header(bytes: &mut Vec<u8>, header: ProgramHeader, align: u64) {
    bytes.extend_from_slice(&header.kind.to_le_bytes());
    bytes.extend_from_slice(&header.flags.to_le_bytes());
    bytes.extend_from_slice(&header.offset.to_le_bytes());
    // Загрузчик не читает физический адрес, он равен виртуальному.
    bytes.extend_from_slice(&header.va_base.to_le_bytes());
    bytes.extend_from_slice(&header.va_base.to_le_bytes());
    bytes.extend_from_slice(&header.file_size.to_le_bytes());
    bytes.extend_from_slice(&header.mem_size.to_le_bytes());
    bytes.extend_from_slice(&align.to_le_bytes());
}

fn push_section_header(bytes: &mut Vec<u8>, name: u32, kind: u32, offset: u64, size: u64) {
    bytes.extend_from_slice(&name.to_le_bytes());
    bytes.extend_from_slice(&kind.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&offset.to_le_bytes());
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
}
