//! Handler-ы Thread-syscall'ов: create/self/exit_code/terminate.
//!
//! `ThreadExit` живёт в [`bridge`](super::bridge) рядом с диспатчером,
//! т.к. имеет not-returning контракт.

use capability::{CapabilityTarget, Rights, UserThreadEntry, runtime};
use collections::LockCell;
use syscall::SyscallError;

use super::{
    bridge::parse_handle_id,
    error::{map_ipc_error, map_spawn_error},
    process::{commit_object_handle, exit_code_from_arg, install_object_handle},
    runtime::runtime as syscall_runtime,
};

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
        .with_lock(|tbl| tbl.get_process(id, Rights::WRITE))
        .map_err(map_ipc_error)?;

    let entry = UserThreadEntry {
        entry_pc,
        user_sp,
        arg,
        priority,
    };

    let reservation = table
        .with_lock(capability::HandleTable::reserve_slot)
        .map_err(map_ipc_error)?;
    let thread = match syscall_runtime().create_user_thread(&process, entry) {
        Ok(t) => t,
        Err(e) => {
            table.with_lock(|tbl| tbl.release_reservation(reservation));
            return Err(map_spawn_error(e));
        }
    };
    Ok(commit_object_handle(
        &table,
        reservation,
        CapabilityTarget::Thread(thread),
    ))
}

pub fn sys_thread_self() -> Result<u64, SyscallError> {
    let thread = syscall_runtime()
        .current_thread_object()
        .ok_or(SyscallError::WrongType)?;
    install_object_handle(CapabilityTarget::Thread(thread))
}

/// Возвращает user-VA per-thread IPC-буфера. Kernel-поток или отсутствие
/// буфера - `WrongType`.
pub fn sys_ipc_buffer_addr() -> Result<u64, SyscallError> {
    syscall_runtime()
        .current_ipc_buffer_va()
        .ok_or(SyscallError::WrongType)
}

pub fn sys_thread_exit_code(handle: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let thread = table
        .with_lock(|tbl| tbl.get_thread(id, Rights::READ))
        .map_err(map_ipc_error)?;
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
        .with_lock(|tbl| tbl.get_thread(id, Rights::WRITE))
        .map_err(map_ipc_error)?;

    // Терминирование собственного потока через handle отвергается:
    // self-exit предусмотрен через `ThreadExit` (0x52), который
    // выполняет context switch.
    if let Some(current) = syscall_runtime().current_thread_object()
        && alloc::sync::Arc::ptr_eq(&current, &thread)
    {
        return Err(SyscallError::AccessDenied);
    }

    syscall_runtime()
        .terminate_thread(&thread, code)
        .map_err(map_ipc_error)?;
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
