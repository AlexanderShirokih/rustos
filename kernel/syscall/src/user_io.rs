//! Общие хелперы для копирования между kernel- и user-памятью в
//! syscall-handler'ах. Под капотом - [`MemoryMapper::copy_user_in`] /
//! [`copy_user_out`]; здесь - валидация user-указателя и единое
//! отображение ошибок копирования в [`SyscallError`].
//!
//! [`MemoryMapper::copy_user_in`]: memory::memory_mapper::MemoryMapper::copy_user_in
//! [`copy_user_out`]: memory::memory_mapper::MemoryMapper::copy_user_out

use memory::{UserVmContext, memory_mapper::UserCopyError, virtual_address::VirtualAddress};

use super::error::SyscallError;

/// Грубая проверка user-указателя: пустой буфер пропускаем без
/// валидации, иначе адрес не должен быть нулевым.
pub(super) fn validate_user_ptr(va: u64, len: usize) -> Result<(), SyscallError> {
    if len == 0 {
        return Ok(());
    }
    if va == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    Ok(())
}

pub(super) fn copy_in(
    user_vm: &UserVmContext,
    va: u64,
    dst: &mut [u8],
) -> Result<(), SyscallError> {
    let va_usize = usize::try_from(va).map_err(|_| SyscallError::InvalidArgument)?;
    user_vm
        .mapper()
        .copy_user_in(VirtualAddress::new(va_usize), dst)
        .map_err(user_copy_err)
}

/// Копирует `src` в user-память по адресу `va`.
pub(super) fn copy_out(user_vm: &UserVmContext, va: u64, src: &[u8]) -> Result<(), SyscallError> {
    let va_usize = usize::try_from(va).map_err(|_| SyscallError::InvalidArgument)?;
    user_vm
        .mapper()
        .copy_user_out(VirtualAddress::new(va_usize), src)
        .map_err(user_copy_err)
}

/// Все варианты [`UserCopyError`] (адрес не маппирован, нет нужных прав)
/// сворачиваем в единый [`SyscallError::InvalidArgument`]: с точки зрения
/// caller'а user-указатель в любом случае непригоден.
pub(super) fn user_copy_err(_e: UserCopyError) -> SyscallError {
    SyscallError::InvalidArgument
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_user_ptr_zero_va_zero_len_ok() {
        assert!(validate_user_ptr(0, 0).is_ok());
    }

    #[test]
    fn validate_user_ptr_zero_va_nonzero_len_invalid() {
        assert_eq!(validate_user_ptr(0, 8), Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn validate_user_ptr_nonzero_va_with_zero_len_ok() {
        assert!(validate_user_ptr(0x1000, 0).is_ok());
    }

    #[test]
    fn validate_user_ptr_nonzero_va_nonzero_len_ok() {
        assert!(validate_user_ptr(0x1000, 8).is_ok());
    }

    #[test]
    fn user_copy_err_maps_to_invalid_argument() {
        assert_eq!(
            user_copy_err(UserCopyError::NotMapped),
            SyscallError::InvalidArgument
        );
        assert_eq!(
            user_copy_err(UserCopyError::AccessDenied),
            SyscallError::InvalidArgument
        );
    }
}
