//! Загрузка программы из её [`EntryView`] в новый процесс из userspace.
//!
//! [`load_entry`] минтит свежие регионы под сегменты, копирует в них байты,
//! собирает дескриптор образа и стартует процесс через `ProcessCreate`/
//! `ProcessLoadImage`/`ProcessStart`. Источник байт абстрагирован за
//! `EntryView`: образ initrd сегодня, файл VFS позже.

#![no_std]
#![allow(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

use runtime::{
    Error, Mapping, MemoryAccess, MemoryRegion, OwnedHandle, Priority, Process, Resource, Result,
    ThreadEntry, UserMemFlags,
};
use syscall::{
    SEGMENT_ABI_VERSION, SyscallError, USER_VA_END, UserImageDescAbi, UserSegmentAbi,
    encode_image_desc, encode_segment,
};
use userland::{EntryView, SegmentPermissions, SegmentView, USERLAND_PAGE_SIZE, user_vm_window};

/// Загружает `entry` в новый процесс и стартует его первый поток.
///
/// Сегменты копируются в свежие приватные регионы. `bootstrap` - стартовый
/// хэндл-канал. `Ok` означает, что хэндл передан потомку.
pub fn load_entry<'a, E: EntryView<'a>>(
    entry: &E,
    resource: &Resource,
    bootstrap: OwnedHandle,
) -> Result<Process> {
    // Регионы, маппинги и буфер сегментов держим живыми до возврата из
    // `start`: регионы должны существовать до установки в child-AS.
    let mut regions: Vec<MemoryRegion> = Vec::new();
    let mut mappings: Vec<Mapping> = Vec::new();
    let mut segment_abi: Vec<u8> = Vec::new();
    let mut segment_count: u32 = 0;

    for seg in entry.segments() {
        let mapped = round_up_to_page(seg.mem_size());
        let region = resource.create_virtual(mapped, MemoryAccess::RWX)?;
        let mapping = region.map(mapped, UserMemFlags::ReadWrite)?;

        let bytes = seg.bytes();
        let dst = usize::try_from(mapping.va()).expect("positive va fits usize") as *mut u8;
        // SAFETY: map выдал RW-маппинг размером `mapped` >= `mem_size` >=
        // `bytes.len()` (инвариант валидного образа); пишем в его границах.
        // Хвост за `bytes` остаётся нулевым (регион зануляется при первом map).
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        }

        let abi = UserSegmentAbi {
            region_handle: region.handle().as_raw().raw(),
            flags: permissions_to_flags(seg.permissions()).raw() as u32,
            va_base: seg.va_base(),
            mapped_size: mapped,
            reserved: 0,
        };
        segment_abi.extend_from_slice(&encode_segment(&abi));

        regions.push(region);
        mappings.push(mapping);
        segment_count += 1;
    }

    // Стек - на потолке user-AS; окно кучи (user_vm) - промежуток между концом
    // сегментов и низом стека, выведенный из самого образа.
    let stack_top = USER_VA_END;
    let window = user_vm_window(
        entry.segments().map(|seg| (seg.va_base(), seg.mem_size())),
        stack_top,
        entry.stack_size(),
    )
    .ok_or(Error::Syscall(SyscallError::InvalidArgument))?;

    let desc = UserImageDescAbi {
        version: SEGMENT_ABI_VERSION,
        segment_count,
        segments_va: segment_abi.as_ptr() as u64,
        entry_va: entry.entry_va(),
        user_stack_top: stack_top,
        user_stack_size: entry.stack_size(),
        user_vm_base: window.start,
        user_vm_size: window.end - window.start,
    };

    let process = Process::create(entry.name())?;
    process.load_image(&encode_image_desc(&desc))?;
    let thread = process.start(
        ThreadEntry {
            entry_pc: entry.entry_va(),
            user_sp: stack_top,
            arg: 0,
            priority: Priority::new(1),
        },
        bootstrap,
    )?;

    // Регионы и маппинги прочитаны ядром при load_image/start; thread-handle
    // здесь не нужен (процесс ждут по его handle).
    drop(thread);
    drop(mappings);
    drop(regions);
    Ok(process)
}

fn permissions_to_flags(permissions: SegmentPermissions) -> UserMemFlags {
    match permissions {
        SegmentPermissions::ReadWrite => UserMemFlags::ReadWrite,
        SegmentPermissions::ReadOnly => UserMemFlags::ReadOnly,
        SegmentPermissions::ReadExecute => UserMemFlags::ReadExecute,
    }
}

fn round_up_to_page(value: u64) -> u64 {
    value.div_ceil(USERLAND_PAGE_SIZE) * USERLAND_PAGE_SIZE
}
