//! Handler-ы memory-syscall'ов.
//!
//! Каждый handler берёт user-VM текущего процесса через
//! [`SyscallRuntime::current_user_vm`], валидирует аргументы, делегирует
//! работу в [`UserVmAllocator`] + [`MemoryMapper`] и возвращает результат
//! в формате [`SyscallError`] / `u64`.
//!
//! ABI флагов памяти упрощён до 3 вариантов: 0 = RW, 1 = RO, 2 = RX.
//!
//! [`UserVmAllocator`]: memory::user_vm_allocator::UserVmAllocator
//! [`MemoryMapper`]: memory::memory_mapper::MemoryMapper
//! [`SyscallRuntime::current_user_vm`]: crate::SyscallRuntime::current_user_vm

use core::num::NonZeroUsize;

use collections::LockCell;
use memory::{
    memory_mapper::{MemoryMappingError, MemoryRemappingError},
    user_vm_allocator::{UserVmAllocateError, UserVmRegionError},
    virtual_address::PageAlignedVirtualAddress,
};

use super::{error::SyscallError, runtime::runtime};
use crate::UserMemFlags;

/// Декодирует сырой `u64`-аргумент в [`UserMemFlags`], мапя ошибку в
/// [`SyscallError::InvalidArgument`].
fn parse_flags(raw: u64) -> Result<UserMemFlags, SyscallError> {
    UserMemFlags::from_raw(raw).map_err(|_| SyscallError::InvalidArgument)
}

/// `vm_allocate(size_bytes, flags) -> va`
///
/// Аллоцирует свежие фреймы из `FrameAllocator`, маппит их в свободный
/// регион user-VA текущего процесса и возвращает базовый VA.
///
/// # Ошибки
/// - [`SyscallError::WrongType`] - у текущего процесса нет user-AS.
/// - [`SyscallError::InvalidArgument`] - `size_bytes == 0`, не кратен
///   странице, либо неизвестный код флагов.
/// - [`SyscallError::OutOfMemory`] - нет свободного user-VA либо
///   аллокатор фреймов исчерпан.
pub fn sys_memory_allocate(size_bytes: u64, flags_raw: u64) -> Result<u64, SyscallError> {
    let size = parse_size(size_bytes)?;
    let flags = parse_flags(flags_raw)?.to_mem_flags();

    let user_vm = runtime().current_user_vm().ok_or(SyscallError::WrongType)?;

    let region = user_vm
        .allocator()
        .with_lock(|alloc| alloc.allocate(size, flags))
        .map_err(allocate_err_to_syscall)?;

    if let Err(e) = user_vm
        .mapper()
        .map(region.base(), region.pages(), &[], flags)
    {
        // mapper.map транзакционен: при ошибке ни одной leaf-страницы в
        // page-tables не остаётся, фреймы возвращены аллокатору. Снимаем
        // регион с учёта, чтобы и bump-указатель аллокатора откатился -
        // VA пойдёт под повторный allocate.
        let pages = NonZeroUsize::new(region.pages()).expect("allocate region has > 0 pages");
        user_vm.allocator().with_lock(|alloc| {
            let released = alloc.release_pending(region.base(), pages);
            debug_assert!(released, "rollback of just-allocated region must succeed");
        });
        return Err(map_err_to_syscall(&e));
    }

    Ok(region.base().as_usize() as u64)
}

/// `vm_remap(va, size_bytes, flags) -> ()`
///
/// Перемаппит уже выделенный регион с новыми флагами доступа. Размер
/// должен полностью совпадать с зарегистрированным регионом - частичный
/// remap не поддерживается (упрощает реестр и инвалидацию TLB).
pub fn sys_memory_remap(va_raw: u64, size_bytes: u64, flags_raw: u64) -> Result<u64, SyscallError> {
    let size = parse_size(size_bytes)?;
    let va_usize = usize::try_from(va_raw).map_err(|_| SyscallError::InvalidArgument)?;
    let flags = parse_flags(flags_raw)?.to_mem_flags();
    let base =
        PageAlignedVirtualAddress::from_usize(va_usize).ok_or(SyscallError::InvalidArgument)?;

    let user_vm = runtime().current_user_vm().ok_or(SyscallError::WrongType)?;

    // Сначала валидируем регистрацию региона без мутаций. Затем правим
    // page-tables. И только в случае успеха обновляем флаги в реестре -
    // так при провале mapper.remap состояния остаются согласованными.
    user_vm
        .allocator()
        .with_lock(|alloc| alloc.lookup(base, size))
        .map_err(region_err_to_syscall)?;

    user_vm
        .mapper()
        .remap(base, size.get(), flags)
        .map_err(|e| remap_err_to_syscall(&e))?;

    user_vm.allocator().with_lock(|alloc| {
        let result = alloc.set_flags(base, size, flags);
        debug_assert!(result.is_ok(), "region must remain after successful remap");
    });

    Ok(0)
}

fn parse_size(size_bytes: u64) -> Result<NonZeroUsize, SyscallError> {
    let size = usize::try_from(size_bytes).map_err(|_| SyscallError::InvalidArgument)?;
    NonZeroUsize::new(size).ok_or(SyscallError::InvalidArgument)
}

fn allocate_err_to_syscall(err: UserVmAllocateError) -> SyscallError {
    match err {
        UserVmAllocateError::UnalignedSize => SyscallError::InvalidArgument,
        UserVmAllocateError::NotEnoughSpace | UserVmAllocateError::OutOfSlots => {
            SyscallError::OutOfMemory
        }
    }
}

fn map_err_to_syscall(err: &MemoryMappingError) -> SyscallError {
    match err {
        MemoryMappingError::OutOfMemory => SyscallError::OutOfMemory,
        MemoryMappingError::AlreadyMapped | MemoryMappingError::VirtualMappingError => {
            SyscallError::InvalidArgument
        }
    }
}

fn region_err_to_syscall(err: UserVmRegionError) -> SyscallError {
    match err {
        UserVmRegionError::NotFound => SyscallError::NotFound,
        UserVmRegionError::UnalignedSize => SyscallError::InvalidArgument,
    }
}

fn remap_err_to_syscall(err: &MemoryRemappingError) -> SyscallError {
    match err {
        MemoryRemappingError::NotMapped => SyscallError::NotFound,
        MemoryRemappingError::MisalignedRange | MemoryRemappingError::UnsupportedBlockMapping => {
            SyscallError::InvalidArgument
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_flags_unknown_maps_to_invalid_argument() {
        assert_eq!(parse_flags(3), Err(SyscallError::InvalidArgument));
        assert_eq!(parse_flags(u64::MAX), Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn allocate_err_mapping() {
        assert_eq!(
            allocate_err_to_syscall(UserVmAllocateError::UnalignedSize),
            SyscallError::InvalidArgument
        );
        assert_eq!(
            allocate_err_to_syscall(UserVmAllocateError::NotEnoughSpace),
            SyscallError::OutOfMemory
        );
        assert_eq!(
            allocate_err_to_syscall(UserVmAllocateError::OutOfSlots),
            SyscallError::OutOfMemory
        );
    }

    #[test]
    fn region_err_mapping() {
        assert_eq!(
            region_err_to_syscall(UserVmRegionError::NotFound),
            SyscallError::NotFound
        );
        assert_eq!(
            region_err_to_syscall(UserVmRegionError::UnalignedSize),
            SyscallError::InvalidArgument
        );
    }

    #[test]
    fn parse_size_zero_is_invalid() {
        assert_eq!(parse_size(0), Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn parse_size_nonzero_passes() {
        assert_eq!(parse_size(4096).map(NonZeroUsize::get), Ok(4096));
    }

    #[test]
    fn map_err_mapping() {
        assert_eq!(
            map_err_to_syscall(&MemoryMappingError::OutOfMemory),
            SyscallError::OutOfMemory
        );
        assert_eq!(
            map_err_to_syscall(&MemoryMappingError::AlreadyMapped),
            SyscallError::InvalidArgument
        );
        assert_eq!(
            map_err_to_syscall(&MemoryMappingError::VirtualMappingError),
            SyscallError::InvalidArgument
        );
    }

    #[test]
    fn remap_err_mapping() {
        assert_eq!(
            remap_err_to_syscall(&MemoryRemappingError::NotMapped),
            SyscallError::NotFound
        );
        assert_eq!(
            remap_err_to_syscall(&MemoryRemappingError::MisalignedRange),
            SyscallError::InvalidArgument
        );
        assert_eq!(
            remap_err_to_syscall(&MemoryRemappingError::UnsupportedBlockMapping),
            SyscallError::InvalidArgument
        );
    }

    /// Гарантия: размер страницы - 4К, как и принято всем ядром. Если
    /// этот инвариант кто-то изменит, syscall-ABI должен измениться явно.
    #[test]
    fn page_alignment_is_4k() {
        use memory::aligned::Aligned;
        assert_eq!(PageAlignedVirtualAddress::ALIGNMENT, 4096);
    }
}
