//! Загрузка программы из её [`ProgramView`] в новый процесс из userspace.
//!
//! [`load_program`] создаёт процесс программы в регионах, оплаченных ресурсом вызывающего, и
//! передаёт ему одну начальную capability. Отказ после создания процесса завершает процесс.

#![no_std]
#![allow(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

use kernel_objects::{
    Error, Mapping, MemoryRegion, OwnedHandle, Process, Resource, Result, ThreadEntry,
};
use runtime::{MemoryAccess, Priority, Timeout, UserMemFlags};
use syscall::{
    ProcessName, SEGMENT_ABI_VERSION, SyscallError, USER_VA_END, UserImageDescAbi, UserSegmentAbi,
    encode_image_desc, encode_segment,
};
use userland::{ProgramView, SegmentPermissions, SegmentView, USERLAND_PAGE_SIZE, user_vm_window};

const STARTUP_FAILURE_EXIT_CODE: u32 = 1;

struct PendingProcess {
    process: Option<Process>,
}

impl PendingProcess {
    fn new(process: Process) -> Self {
        Self {
            process: Some(process),
        }
    }

    fn process(&self) -> Result<&Process> {
        self.process.as_ref().ok_or_else(state_taken)
    }

    fn finish(mut self) -> Result<Process> {
        self.process.take().ok_or_else(state_taken)
    }

    fn fail<T>(self, error: Error) -> Result<T> {
        match self.rollback() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(cleanup),
        }
    }

    fn rollback(mut self) -> Result<()> {
        let Some(process) = self.process.as_ref() else {
            return Ok(());
        };
        terminate_and_join(process)?;
        self.process = None;
        Ok(())
    }
}

impl Drop for PendingProcess {
    fn drop(&mut self) {
        if let Some(process) = self.process.as_ref() {
            let _ = process.terminate(STARTUP_FAILURE_EXIT_CODE);
        }
    }
}

fn state_taken() -> Error {
    Error::Syscall(SyscallError::BadHandle)
}

fn terminate_and_join(process: &Process) -> Result<()> {
    process.terminate(STARTUP_FAILURE_EXIT_CODE)?;
    process.join(Timeout::POLL)
}

/// Загружает программу в новый процесс с именем `name`, передаёт `initial` и запускает поток.
/// `resource` оплачивает приватные копии сегментов и служит ресурсом потомка.
/// `Ok` означает передачу хендла потомку. Отказ после создания процесса завершает его.
pub fn load_program<'a, E: ProgramView<'a>, K: kernel_objects::ObjectKind>(
    program: &E,
    name: ProcessName<'_>,
    resource: &Resource,
    initial: OwnedHandle<K>,
) -> Result<Process> {
    // Регионы, маппинги и буфер сегментов держим живыми до возврата из
    // `start`: регионы должны существовать до установки в child-AS.
    let mut regions: Vec<MemoryRegion> = Vec::new();
    let mut mappings: Vec<Mapping> = Vec::new();
    let mut segment_abi: Vec<u8> = Vec::new();
    let mut segment_count: u32 = 0;

    for seg in program.segments() {
        let mapped = round_up_to_page(seg.mem_size());
        let region = resource.create_virtual(mapped, MemoryAccess::RWX)?;
        let mapping = region.map(mapped, UserMemFlags::ReadWrite)?;

        let bytes = seg.bytes();
        let dst = mapping.va() as *mut u8;
        // SAFETY: map выдал RW-маппинг размером `mapped` >= `mem_size` >=
        // `bytes.len()` (инвариант валидного образа); пишем в его границах.
        // Хвост за `bytes` остаётся нулевым (регион зануляется при первом map).
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        }

        let abi = UserSegmentAbi {
            region_handle: region.as_raw().raw(),
            #[allow(clippy::cast_possible_truncation)]
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
    let stack_size = round_up_to_page(program.stack_size());
    let window = user_vm_window(
        program
            .segments()
            .map(|seg| (seg.va_base(), seg.mem_size())),
        stack_top,
        stack_size,
    )
    .ok_or(Error::Syscall(SyscallError::InvalidArgument))?;

    // Стек - обычный сегмент, который ядро отдельно не выделяет, а его страницы
    // оплачивает тот же `resource`, что и остальной образ.
    let stack = resource.create_virtual(stack_size, MemoryAccess::RW)?;
    let stack_abi = UserSegmentAbi {
        region_handle: stack.as_raw().raw(),
        #[allow(clippy::cast_possible_truncation)]
        flags: UserMemFlags::ReadWrite.raw() as u32,
        va_base: stack_top - stack_size,
        mapped_size: stack_size,
        reserved: 0,
    };
    segment_abi.extend_from_slice(&encode_segment(&stack_abi));
    segment_count += 1;
    regions.push(stack);

    let desc = UserImageDescAbi {
        version: SEGMENT_ABI_VERSION,
        segment_count,
        segments_va: segment_abi.as_ptr() as u64,
        entry_va: program.entry_va(),
        user_vm_base: window.start,
        user_vm_size: window.end - window.start,
    };

    let pending = PendingProcess::new(Process::create(name)?);
    if let Err(error) = pending.process()?.load_image(&encode_image_desc(&desc)) {
        return pending.fail(error);
    }
    let thread = match pending.process()?.start(
        ThreadEntry {
            entry_pc: program.entry_va(),
            user_sp: stack_top,
            arg: 0,
            priority: Priority::new(1),
        },
        initial,
        Some(resource),
    ) {
        Ok(thread) => thread,
        Err(error) => return pending.fail(error),
    };

    // Регионы и маппинги прочитаны ядром при load_image/start; thread-handle
    // здесь не нужен (процесс ждут по его handle).
    drop(thread);
    drop(mappings);
    drop(regions);
    pending.finish()
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
