//! Хелперы копирования user -> kernel в syscall-обработчиках:
//! валидация указателей и отображение [`UserCopyError`] в [`SyscallError`].

use memory::{UserVmContext, memory_mapper::UserCopyError, virtual_address::VirtualAddress};
use syscall::SyscallError;

pub(super) fn validate_user_ptr(va: u64, len: usize) -> Result<(), SyscallError> {
    if len == 0 {
        return Ok(());
    }
    if va == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    // `va + len` не заворачивается за границу адресного пространства.
    let len_u64 = u64::try_from(len).map_err(|_| SyscallError::InvalidArgument)?;
    va.checked_add(len_u64)
        .ok_or(SyscallError::InvalidArgument)?;
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

pub(super) fn user_copy_err(_e: UserCopyError) -> SyscallError {
    SyscallError::InvalidArgument
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use kbase::MutexCell;
    use memory::{
        MemFlags, PAGE_SIZE, memory_mapper::MemoryMapper, testing::PageMapper,
        user_vm_allocator::UserVmAllocator, virtual_address::PageAlignedVirtualAddress,
    };

    use super::*;

    const PAGE_END: usize = 0x4000;

    fn make_ctx(bytes: &[u8]) -> (UserVmContext, u64) {
        let page = PAGE_END - PAGE_SIZE.get();
        let mut init = std::vec![0u8; PAGE_SIZE.get() - bytes.len()];
        init.extend_from_slice(bytes);

        let mapper = Arc::new(PageMapper::new());
        mapper
            .map(
                PageAlignedVirtualAddress::from_usize(page).unwrap(),
                1,
                &init,
                MemFlags::user_rw(),
            )
            .unwrap();
        let allocator = Arc::new(MutexCell::new(UserVmAllocator::new(
            PageAlignedVirtualAddress::from_usize(0x1000_0000).unwrap(),
            VirtualAddress::new(0x1100_0000),
        )));
        let va = (PAGE_END - bytes.len()) as u64;
        (UserVmContext::new(mapper, allocator), va)
    }

    #[test]
    fn copy_in_reads_user_bytes() {
        let (ctx, va) = make_ctx(&[1, 2, 3, 4]);
        let mut dst = [0u8; 4];
        copy_in(&ctx, va, &mut dst).expect("copy in ok");
        assert_eq!(dst, [1, 2, 3, 4]);
    }

    #[test]
    fn copy_in_out_of_bounds_is_invalid_argument() {
        let (ctx, va) = make_ctx(&[1, 2, 3, 4]);
        let mut dst = [0u8; 8];
        assert_eq!(
            copy_in(&ctx, va, &mut dst),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn copy_in_unmapped_va_is_invalid_argument() {
        let (ctx, _) = make_ctx(&[1, 2, 3, 4]);
        let mut dst = [0u8; 4];
        assert_eq!(
            copy_in(&ctx, u64::MAX, &mut dst),
            Err(SyscallError::InvalidArgument)
        );
    }

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
    fn validate_user_ptr_va_plus_len_overflow_is_invalid() {
        assert_eq!(
            validate_user_ptr(u64::MAX, 1),
            Err(SyscallError::InvalidArgument)
        );
        assert_eq!(
            validate_user_ptr(u64::MAX - 3, 8),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn validate_user_ptr_range_ending_exactly_at_max_is_ok() {
        assert!(validate_user_ptr(u64::MAX - 8, 8).is_ok());
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
