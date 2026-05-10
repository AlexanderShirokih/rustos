//! Handler-ы Process-syscall'ов: create/self/exit_code/terminate.
//!
//! Парсят аргументы, пробрасывают в `kobject` API и регистрируют новые
//! handle'ы в текущей handle-table вызывающего процесса.

use collections::LockCell;
use kobject::{Handle, KObject, Rights, install_handle, runtime};
use memory::{UserVmContext, memory_mapper::UserCopyError, virtual_address::VirtualAddress};

use super::{bridge::parse_handle_id, error::SyscallError, runtime::runtime as syscall_runtime};

/// Максимальная длина имени процесса, передаваемая через user-память.
/// Содержимое в текущей реализации не сохраняется, валидация только
/// проверяет, что аргументы укладываются в разумные границы.
const MAX_PROCESS_NAME_LEN: usize = 64;

/// Sentinel-имя для процессов, созданных через syscall: пользовательское
/// имя в текущей реализации не сохраняется.
const USER_PROCESS_NAME: &str = "<user>";

pub fn sys_process_create(name_va: u64, name_len: u64) -> Result<u64, SyscallError> {
    let len = usize::try_from(name_len).map_err(|_| SyscallError::InvalidArgument)?;
    if len > MAX_PROCESS_NAME_LEN {
        return Err(SyscallError::InvalidArgument);
    }
    if len > 0 {
        if name_va == 0 {
            return Err(SyscallError::InvalidArgument);
        }
        let user_vm = syscall_runtime()
            .current_user_vm()
            .ok_or(SyscallError::WrongType)?;
        let mut buf = [0u8; MAX_PROCESS_NAME_LEN];
        copy_in(&user_vm, name_va, &mut buf[..len])?;
    }

    let process = kobject::create_empty_process(USER_PROCESS_NAME)?;
    let ko = KObject::Process(process);
    let rights = Rights::defaults_for(&ko);
    let handle_id = install_handle(Handle::new(ko, rights))?;
    Ok(u64::from(handle_id.raw().get()))
}

pub fn sys_process_self() -> Result<u64, SyscallError> {
    let process = runtime()
        .current_process_object()
        .ok_or(SyscallError::WrongType)?;
    let ko = KObject::Process(process);
    let rights = Rights::defaults_for(&ko);
    let handle_id = install_handle(Handle::new(ko, rights))?;
    Ok(u64::from(handle_id.raw().get()))
}

pub fn sys_process_exit_code(handle: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let process = table
        .with_lock(|tbl| tbl.get_process(id, Rights::INSPECT))
        .map_err(SyscallError::from)?;
    let code = process.exit_code();
    Ok(u64::from(code.cast_unsigned()))
}

pub fn sys_process_terminate(handle: u64, exit_code: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let code = exit_code_from_arg(exit_code);
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let process = table
        .with_lock(|tbl| tbl.get_process(id, Rights::MANAGE_PROCESS))
        .map_err(SyscallError::from)?;

    // Терминирование собственного процесса через handle отвергается:
    // путь не выполнил бы context switch и dispatcher вернул бы код возврата
    // в уже завершённый поток. Self-exit идёт через `ThreadExit (0x52)` -
    // последний поток процесса автоматически поднимет `PROCESS_TERMINATED`.
    if let Some(current) = runtime().current_process_object()
        && alloc::sync::Arc::ptr_eq(&current, &process)
    {
        return Err(SyscallError::AccessDenied);
    }

    kobject::terminate_process(&process, code)?;
    Ok(0)
}

/// Декодирует младшие 32 бита аргумента в `i32` exit-код. Верхние биты
/// игнорируются - ABI фиксирует exit_code в нижних 32-х.
pub(super) fn exit_code_from_arg(raw: u64) -> i32 {
    let low32 =
        u32::try_from(raw & u64::from(u32::MAX)).expect("masking guarantees value fits into u32");
    low32.cast_signed()
}

fn copy_in(user_vm: &UserVmContext, va: u64, dst: &mut [u8]) -> Result<(), SyscallError> {
    let va_usize = usize::try_from(va).map_err(|_| SyscallError::InvalidArgument)?;
    user_vm
        .mapper()
        .copy_user_in(VirtualAddress::new(va_usize), dst)
        .map_err(user_copy_err)
}

fn user_copy_err(_e: UserCopyError) -> SyscallError {
    SyscallError::InvalidArgument
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_create_with_oversized_name_returns_invalid_argument() {
        assert_eq!(
            sys_process_create(0x1000, (MAX_PROCESS_NAME_LEN as u64) + 1),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn process_create_with_nonzero_len_and_null_va_returns_invalid_argument() {
        assert_eq!(sys_process_create(0, 8), Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn process_terminate_zero_handle_is_invalid_argument() {
        assert_eq!(
            sys_process_terminate(0, 0),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn process_exit_code_zero_handle_is_invalid_argument() {
        assert_eq!(sys_process_exit_code(0), Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn exit_code_from_arg_truncates_to_lower_32_bits() {
        assert_eq!(exit_code_from_arg(0), 0);
        assert_eq!(exit_code_from_arg(42), 42);
        // -1 in 32-bit two's complement
        assert_eq!(exit_code_from_arg(0xFFFF_FFFF), -1);
        // upper bits ignored
        assert_eq!(exit_code_from_arg(0x1_0000_0000 | 7), 7);
    }
}
