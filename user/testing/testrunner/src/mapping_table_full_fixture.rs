//! Программа-фикстура заполнения таблицы отображений. Выделяет анонимные страницы до отказа и
//! сверяет его код, затем отказы отображения стартового региона и создания потока, которому
//! ядро отображает IPC-буфер, и выделение после снятия одного отображения. Код выхода называет
//! первую несовпавшую проверку. После начала заполнения фикстура не выделяет из кучи, потому
//! что куче нужны те же слоты таблицы.
#![no_std]
#![no_main]

use kernel_objects::MemoryRegion;
use runtime::{
    ExitCode, FAILED_EXIT_CODE, Priority, SUCCESS_EXIT_CODE, UserMemFlags, memory_allocate,
    memory_free, memory_map, metering_handle, process_self, thread_create, thread_exit,
};
use syscall::SyscallError;
use userland::USERLAND_PAGE_SIZE;

const FILL_ATTEMPTS_MAX: usize = 4096;

const THREAD_PRIORITY: Priority = Priority::new(1);

const NO_METERING_EXIT_CODE: u32 = FAILED_EXIT_CODE.cast_unsigned() + 1;
const NO_PROCESS_EXIT_CODE: u32 = 3;
const NO_REFUSAL_EXIT_CODE: u32 = 4;
const FIRST_ALLOCATION_REFUSED_EXIT_CODE: u32 = 5;
const ALLOCATE_REFUSAL_EXIT_CODE: u32 = 6;
const MAP_REFUSAL_EXIT_CODE: u32 = 7;
const THREAD_REFUSAL_EXIT_CODE: u32 = 8;
const FREE_FAILED_EXIT_CODE: u32 = 9;
const NO_ROOM_AFTER_FREE_EXIT_CODE: u32 = 10;

#[program_entry::process]
fn run(probe: &MemoryRegion) -> ExitCode {
    let Some(resource) = metering_handle() else {
        return ExitCode::from(NO_METERING_EXIT_CODE);
    };
    let Ok(process) = process_self() else {
        return ExitCode::from(NO_PROCESS_EXIT_CODE);
    };
    let allocate = || memory_allocate(resource, USERLAND_PAGE_SIZE, UserMemFlags::ReadWrite.raw());

    let mut last = None;
    let mut refusal = None;
    for _ in 0..FILL_ATTEMPTS_MAX {
        let ret = allocate();
        match SyscallError::from_return(ret) {
            Some(error) => {
                refusal = Some(error);
                break;
            }
            None => last = Some(ret),
        }
    }

    let Some(refusal) = refusal else {
        return ExitCode::from(NO_REFUSAL_EXIT_CODE);
    };
    let Some(Ok(last)) = last.map(u64::try_from) else {
        return ExitCode::from(FIRST_ALLOCATION_REFUSED_EXIT_CODE);
    };
    if refusal != SyscallError::OutOfMappings {
        return ExitCode::from(ALLOCATE_REFUSAL_EXIT_CODE);
    }

    let mapped = memory_map(
        probe.as_raw(),
        USERLAND_PAGE_SIZE,
        UserMemFlags::ReadWrite.raw(),
    );
    if SyscallError::from_return(mapped) != Some(SyscallError::OutOfMappings) {
        return ExitCode::from(MAP_REFUSAL_EXIT_CODE);
    }

    // Стек потока лежит на последней выданной странице, потому что при заполненной таблице
    // другую не выделить.
    let thread = thread_create(
        process.as_raw(),
        stray_thread as *const () as u64,
        last + USERLAND_PAGE_SIZE,
        0,
        u64::from(THREAD_PRIORITY),
    );
    if thread.err() != Some(SyscallError::OutOfMappings) {
        return ExitCode::from(THREAD_REFUSAL_EXIT_CODE);
    }

    if memory_free(last, USERLAND_PAGE_SIZE) != 0 {
        return ExitCode::from(FREE_FAILED_EXIT_CODE);
    }
    if SyscallError::from_return(allocate()).is_some() {
        return ExitCode::from(NO_ROOM_AFTER_FREE_EXIT_CODE);
    }

    ExitCode::from(SUCCESS_EXIT_CODE.cast_unsigned())
}

extern "C" fn stray_thread(_arg: usize) -> ! {
    thread_exit(0)
}
