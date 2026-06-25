//! Хелперы копирования user -> kernel в syscall-handler'ах:
//! валидация указателей и отображение [`UserCopyError`] в [`SyscallError`].

use memory::{UserVmContext, memory_mapper::UserCopyError, virtual_address::VirtualAddress};
use syscall::SyscallError;

/// Грубая проверка user-указателя: пустой буфер пропускаем без
/// валидации, иначе адрес не должен быть нулевым, а диапазон `[va, va+len)`
/// не должен переполнять адресное пространство.
pub(super) fn validate_user_ptr(va: u64, len: usize) -> Result<(), SyscallError> {
    if len == 0 {
        return Ok(());
    }
    if va == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    // `va + len` не должен заворачиваться за границу адресного пространства:
    // иначе валидация прошла бы, но диапазон был бы бессмысленным.
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

/// Все варианты [`UserCopyError`] -> [`SyscallError::InvalidArgument`]:
/// с точки зрения caller'а user-указатель непригоден в любом случае.
pub(super) fn user_copy_err(_e: UserCopyError) -> SyscallError {
    SyscallError::InvalidArgument
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use collections::MutexCell;
    use memory::{
        MemFlags,
        memory_mapper::{
            AddressSpaceHandle, AddressSpaceTag, MemoryMappingError, MemoryRemappingError,
            MemoryUnmappingError,
        },
        physical_address::{PageAlignedAddress, PhysicalAddress},
        user_vm_allocator::UserVmAllocator,
        virtual_address::PageAlignedVirtualAddress,
    };

    use super::*;

    // Чтение за пределами буфера -> NotMapped.
    struct CannedMapper {
        base_va: usize,
        writable: bool,
        bytes: std::sync::Mutex<std::vec::Vec<u8>>,
    }

    impl CannedMapper {
        fn range(&self, va: VirtualAddress, len: usize) -> Result<(usize, usize), UserCopyError> {
            let offset = va.as_usize().wrapping_sub(self.base_va);
            let end = offset.checked_add(len).ok_or(UserCopyError::NotMapped)?;
            if va.as_usize() < self.base_va || end > self.bytes.lock().unwrap().len() {
                return Err(UserCopyError::NotMapped);
            }
            Ok((offset, end))
        }
    }

    impl memory::memory_mapper::MemoryMapper for CannedMapper {
        fn map(
            &self,
            _va: PageAlignedVirtualAddress,
            _page_count: usize,
            _init: &[u8],
            _flags: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            unimplemented!("test mapper supports only copy_user_*")
        }

        fn map_exact(
            &self,
            _va: PageAlignedVirtualAddress,
            _pa: PageAlignedAddress,
            _size: usize,
            _flags: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            unimplemented!("test mapper supports only copy_user_*")
        }

        fn unmap(
            &self,
            _va: PageAlignedVirtualAddress,
            _size: usize,
        ) -> Result<(), MemoryUnmappingError> {
            unimplemented!("test mapper supports only copy_user_*")
        }

        fn remap(
            &self,
            _va: PageAlignedVirtualAddress,
            _size: usize,
            _flags: MemFlags,
        ) -> Result<(), MemoryRemappingError> {
            unimplemented!("test mapper supports only copy_user_*")
        }

        fn activate_handle(&self) -> AddressSpaceHandle {
            AddressSpaceHandle::new(PhysicalAddress::new(0), AddressSpaceTag::NONE)
        }

        fn zero_owned_frame(&self, _pa: PageAlignedAddress) {
            unimplemented!("test mapper supports only copy_user_*")
        }

        fn copy_user_in(&self, va: VirtualAddress, dst: &mut [u8]) -> Result<(), UserCopyError> {
            let (offset, end) = self.range(va, dst.len())?;
            dst.copy_from_slice(&self.bytes.lock().unwrap()[offset..end]);
            Ok(())
        }

        fn copy_user_out(&self, va: VirtualAddress, src: &[u8]) -> Result<(), UserCopyError> {
            if !self.writable {
                return Err(UserCopyError::AccessDenied);
            }
            let (offset, end) = self.range(va, src.len())?;
            self.bytes.lock().unwrap()[offset..end].copy_from_slice(src);
            Ok(())
        }

        fn as_any(&self) -> &(dyn core::any::Any + 'static) {
            self
        }
    }

    fn make_ctx(base_va: usize, bytes: std::vec::Vec<u8>, writable: bool) -> UserVmContext {
        let mapper = Arc::new(CannedMapper {
            base_va,
            writable,
            bytes: std::sync::Mutex::new(bytes),
        });
        let allocator = Arc::new(MutexCell::new(UserVmAllocator::new(
            PageAlignedVirtualAddress::from_usize(0x1000_0000).unwrap(),
            VirtualAddress::new(0x1100_0000),
        )));
        UserVmContext::new(mapper, allocator)
    }

    #[test]
    fn copy_in_reads_user_bytes() {
        let ctx = make_ctx(0x4000, std::vec![1, 2, 3, 4], true);
        let mut dst = [0u8; 4];
        copy_in(&ctx, 0x4000, &mut dst).expect("copy in ok");
        assert_eq!(dst, [1, 2, 3, 4]);
    }

    #[test]
    fn copy_in_out_of_bounds_is_invalid_argument() {
        let ctx = make_ctx(0x4000, std::vec![1, 2, 3, 4], true);
        let mut dst = [0u8; 8];
        assert_eq!(
            copy_in(&ctx, 0x4000, &mut dst),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn copy_in_unmapped_va_is_invalid_argument() {
        let ctx = make_ctx(0x4000, std::vec![1, 2, 3, 4], true);
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
