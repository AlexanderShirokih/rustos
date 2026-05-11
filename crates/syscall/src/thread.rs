//! Handler-ы Thread-syscall'ов: create/self/exit_code/terminate.
//!
//! `ThreadExit` живёт в [`bridge`](super::bridge) рядом с диспатчером,
//! т.к. имеет not-returning контракт.

use collections::LockCell;
use kobject::{Handle, KObject, Rights, UserThreadEntry, install_handle, runtime};

use super::{bridge::parse_handle_id, error::SyscallError, process::exit_code_from_arg};

pub fn sys_thread_create(
    process_handle: u64,
    entry_pc: u64,
    user_sp: u64,
    arg: u64,
    priority: u64,
) -> Result<u64, SyscallError> {
    let id = parse_handle_id(process_handle)?;
    let priority = u8::try_from(priority).map_err(|_| SyscallError::InvalidArgument)?;

    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let process = table
        .with_lock(|tbl| tbl.get_process(id, Rights::MANAGE_PROCESS))
        .map_err(SyscallError::from)?;

    let entry = UserThreadEntry {
        entry_pc,
        user_sp,
        arg,
        priority,
    };

    let reservation = table
        .with_lock(kobject::HandleTable::reserve_slot)
        .map_err(SyscallError::from)?;
    let thread = match kobject::create_user_thread(&process, entry) {
        Ok(t) => t,
        Err(e) => {
            table.with_lock(|tbl| tbl.release_reservation(reservation));
            return Err(SyscallError::from(e));
        }
    };
    let ko = KObject::Thread(thread);
    let rights = Rights::defaults_for(&ko);
    let handle_id =
        table.with_lock(|tbl| tbl.commit_reserved(reservation, Handle::new(ko, rights)));
    Ok(u64::from(handle_id.raw().get()))
}

pub fn sys_thread_self() -> Result<u64, SyscallError> {
    let thread = runtime()
        .current_thread_object()
        .ok_or(SyscallError::WrongType)?;
    let ko = KObject::Thread(thread);
    let rights = Rights::defaults_for(&ko);
    let handle_id = install_handle(Handle::new(ko, rights))?;
    Ok(u64::from(handle_id.raw().get()))
}

pub fn sys_thread_exit_code(handle: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let thread = table
        .with_lock(|tbl| tbl.get_thread(id, Rights::INSPECT))
        .map_err(SyscallError::from)?;
    let code = thread.exit_code();
    Ok(u64::from(code.cast_unsigned()))
}

pub fn sys_thread_terminate(handle: u64, exit_code: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let code = exit_code_from_arg(exit_code);
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let thread = table
        .with_lock(|tbl| tbl.get_thread(id, Rights::MANAGE_THREAD))
        .map_err(SyscallError::from)?;

    // Терминирование собственного потока через handle отвергается:
    // self-exit предусмотрен через `ThreadExit` (0x52), который
    // выполняет context switch.
    if let Some(current) = runtime().current_thread_object()
        && alloc::sync::Arc::ptr_eq(&current, &thread)
    {
        return Err(SyscallError::AccessDenied);
    }

    kobject::terminate_thread(&thread, code)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_create_zero_handle_is_invalid_argument() {
        assert_eq!(
            sys_thread_create(0, 0x4000_0000, 0x4001_0000, 0, 1),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn thread_create_oversized_priority_is_invalid_argument() {
        assert_eq!(
            sys_thread_create(1, 0x4000_0000, 0x4001_0000, 0, 0x1_0000),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn thread_terminate_zero_handle_is_invalid_argument() {
        assert_eq!(
            sys_thread_terminate(0, 0),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn thread_exit_code_zero_handle_is_invalid_argument() {
        assert_eq!(sys_thread_exit_code(0), Err(SyscallError::InvalidArgument));
    }
}
