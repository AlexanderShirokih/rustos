//! Handler-ы memory-syscall'ов.
//!
//! Каждый handler берёт user-VM текущего процесса через
//! [`SyscallRuntime::current_user_vm`], валидирует аргументы, делегирует
//! работу в [`UserVmAllocator`] + [`MemoryRegion`] и возвращает результат
//! в формате [`SyscallError`] / `u64`.
//!
//! ABI флагов памяти упрощён до 3 вариантов: 0 = RW, 1 = RO, 2 = RX.
//!
//! [`UserVmAllocator`]: memory::UserVmAllocator
//! [`MemoryRegion`]: memory::MemoryRegion
//! [`SyscallRuntime::current_user_vm`]: crate::SyscallRuntime::current_user_vm

use alloc::sync::Arc;
use core::num::NonZeroUsize;

use collections::LockCell;
use kobject::{Handle, IpcError, KObject, Rights};
use memory::{
    AccessMask, MappingTag, MemoryRegion, RegionCreateError,
    memory_mapper::{MemoryMappingError, MemoryRemappingError, MemoryUnmappingError},
    physical_address::PageAlignedAddress,
    range_allocator::{AllocateError, RangeError},
    virtual_address::PageAlignedVirtualAddress,
};

use super::{
    bridge::{SyscallFrame, parse_handle_id},
    error::{SyscallError, encode_return},
    runtime::runtime,
};
use crate::UserMemFlags;

fn lookup_memory(id: kobject::HandleId, need: Rights) -> Result<Arc<MemoryRegion>, SyscallError> {
    let table = kobject::runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let region = table.with_lock(|tbl| tbl.get_memory(id, need))?;
    Ok(region)
}

fn lookup_memory_grant(
    id: kobject::HandleId,
    need: Rights,
) -> Result<(Arc<MemoryRegion>, AccessMask), SyscallError> {
    let table = kobject::runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    let (region, rights) = table.with_lock(|tbl| tbl.get_memory_with_rights(id, need))?;
    let mut bits: u8 = 0;
    if rights.contains(Rights::READ) {
        bits |= AccessMask::R.bits();
    }
    if rights.contains(Rights::WRITE) {
        bits |= AccessMask::W.bits();
    }
    if rights.contains(Rights::EXECUTE) {
        bits |= AccessMask::X.bits();
    }
    let handle_mask = AccessMask::from_bits_truncate(bits);
    let grant = AccessMask::from_bits_truncate(handle_mask.bits() & region.access_mask().bits());
    Ok((region, grant))
}

const PAGE_SIZE: usize = 4096;

fn parse_flags(raw: u64) -> Result<UserMemFlags, SyscallError> {
    UserMemFlags::from_raw(raw).map_err(|_| SyscallError::InvalidArgument)
}

fn parse_size(size_bytes: u64) -> Result<NonZeroUsize, SyscallError> {
    let size = usize::try_from(size_bytes).map_err(|_| SyscallError::InvalidArgument)?;
    NonZeroUsize::new(size).ok_or(SyscallError::InvalidArgument)
}

/// Парсит `access_mask` из u64-аргумента: только нижние 3 бита R/W/X
/// важны, нулевая маска бессмысленна (регион ни для чего не годится).
fn parse_access_mask(raw: u64) -> Result<AccessMask, SyscallError> {
    let bits = u8::try_from(raw & 0b111).expect("masked to 3 bits");
    let mask = AccessMask::from_bits_truncate(bits);
    if mask.bits() == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    Ok(mask)
}

fn rights_for_access(flags: UserMemFlags) -> Rights {
    match flags {
        UserMemFlags::ReadOnly => Rights::READ,
        UserMemFlags::ReadWrite => Rights::READ | Rights::WRITE,
        UserMemFlags::ReadExecute => Rights::READ | Rights::EXECUTE,
    }
}

fn access_mask_for(flags: UserMemFlags) -> AccessMask {
    match flags {
        UserMemFlags::ReadOnly => AccessMask::R,
        UserMemFlags::ReadWrite => AccessMask::RW,
        UserMemFlags::ReadExecute => AccessMask::RX,
    }
}

/// `memory_create_virtual(auth_handle, size_bytes, access_mask) -> region_handle`
///
/// Минтит новый Virtual `MemoryRegion` под защитой `MemoryAuthority` с
/// правом [`Rights::CREATE_VIRTUAL`]. Регистрирует регион в HandleTable
/// текущего процесса, возвращает свежий handle.
pub fn sys_memory_create_virtual(
    auth_h: u64,
    size_bytes: u64,
    access_raw: u64,
) -> Result<u64, SyscallError> {
    let auth_id = parse_handle_id(auth_h)?;
    let size = parse_size(size_bytes)?;
    let access = parse_access_mask(access_raw)?;
    let pages = NonZeroUsize::new(size.get() / PAGE_SIZE).ok_or(SyscallError::InvalidArgument)?;
    if pages.get() * PAGE_SIZE != size.get() {
        return Err(SyscallError::InvalidArgument);
    }

    let table = kobject::runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let _authority = table.with_lock(|tbl| tbl.get_authority(auth_id, Rights::CREATE_VIRTUAL))?;

    let fa = runtime()
        .frame_allocator()
        .ok_or(SyscallError::OutOfMemory)?;
    let region = MemoryRegion::create_virtual(fa, pages, access)
        .map_err(|e| region_create_err_to_syscall(&e))?;
    let region_arc = Arc::new(region);
    let ko = KObject::Memory(region_arc);
    let handle = Handle::new(ko.clone(), Rights::defaults_for(&ko));
    let id = table.with_lock(|tbl| tbl.insert(handle))?;
    Ok(u64::from(id.raw().get()))
}

/// `memory_create_physical(auth_handle, pa, size_bytes, access_mask) -> region_handle`
///
/// Минтит регион поверх фиксированного PA-диапазона под защитой
/// `MemoryAuthority` с правом [`Rights::CREATE_PHYSICAL`]. Регион не
/// владеет физикой и на drop ничего не возвращает; PA должен быть
/// page-aligned.
pub fn sys_memory_create_physical(
    auth_h: u64,
    pa_raw: u64,
    size_bytes: u64,
    access_raw: u64,
) -> Result<u64, SyscallError> {
    let auth_id = parse_handle_id(auth_h)?;
    let size = parse_size(size_bytes)?;
    let access = parse_access_mask(access_raw)?;
    let pa_usize = usize::try_from(pa_raw).map_err(|_| SyscallError::InvalidArgument)?;
    let pa = PageAlignedAddress::from_usize(pa_usize).ok_or(SyscallError::InvalidArgument)?;

    let table = kobject::runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let _authority = table.with_lock(|tbl| tbl.get_authority(auth_id, Rights::CREATE_PHYSICAL))?;

    let region = MemoryRegion::create_physical(pa, size, access);
    let region_arc = Arc::new(region);
    let ko = KObject::Memory(region_arc);
    let handle = Handle::new(ko.clone(), Rights::defaults_for(&ko));
    let id = table.with_lock(|tbl| tbl.insert(handle))?;
    Ok(u64::from(id.raw().get()))
}

/// `memory_allocate(size, flags) -> va`
///
/// Создаёт анонимный `Virtual` регион нужного размера, оборачивает в `Arc`,
/// аллоцирует свободный VA в `UserVmAllocator`, маппит регион через
/// `MemoryRegion::install`, регистрирует тег. Регион не выкладывается в
/// HandleTable.
pub fn sys_memory_allocate(size_bytes: u64, flags_raw: u64) -> Result<u64, SyscallError> {
    let size = parse_size(size_bytes)?;
    let flags = parse_flags(flags_raw)?;
    let mem_flags = flags.to_mem_flags();
    let access = access_mask_for(flags);

    let user_vm = runtime().current_user_vm().ok_or(SyscallError::WrongType)?;
    let fa = runtime()
        .frame_allocator()
        .ok_or(SyscallError::OutOfMemory)?;

    let pages_count =
        NonZeroUsize::new(size.get() / PAGE_SIZE).ok_or(SyscallError::InvalidArgument)?;
    if pages_count.get() * PAGE_SIZE != size.get() {
        return Err(SyscallError::InvalidArgument);
    }

    let region = MemoryRegion::create_virtual(fa, pages_count, access)
        .map_err(|e| region_create_err_to_syscall(&e))?;
    let region = Arc::new(region);

    // sys_memory_allocate - fastpath без публикации handle'а; caller
    // эффективно владеет полным access-mask только что созданного региона,
    // grant равен region.access_mask().
    let grant = region.access_mask();

    let allocated = user_vm
        .allocator()
        .with_lock(|alloc| {
            alloc.allocate(
                size,
                MappingTag {
                    flags: mem_flags,
                    region: region.clone(),
                    grant,
                },
            )
        })
        .map_err(allocate_err_to_syscall)?;
    let base = allocated.base();

    if let Err(e) = region.install(user_vm.mapper(), base, mem_flags) {
        user_vm.allocator().with_lock(|alloc| {
            let released = alloc.free(base, size);
            debug_assert!(
                released.is_ok(),
                "rollback of just-allocated region must succeed"
            );
        });
        return Err(map_err_to_syscall(&e));
    }

    Ok(base.as_usize() as u64)
}

/// `memory_map(region_handle, size, flags) -> va`
///
/// Маппит существующий регион из HandleTable в свободный VA.
pub fn sys_memory_map(
    handle_raw: u64,
    size_bytes: u64,
    flags_raw: u64,
) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle_raw)?;
    let size = parse_size(size_bytes)?;
    let flags = parse_flags(flags_raw)?;
    let mem_flags = flags.to_mem_flags();
    let need = Rights::MAP | rights_for_access(flags);

    let (region, grant) = lookup_memory_grant(id, need)?;

    if !grant.allows(access_mask_for(flags)) {
        return Err(SyscallError::InvalidArgument);
    }
    if region.size_bytes() != size.get() {
        return Err(SyscallError::InvalidArgument);
    }

    let user_vm = runtime().current_user_vm().ok_or(SyscallError::WrongType)?;

    let allocated = user_vm
        .allocator()
        .with_lock(|alloc| {
            alloc.allocate(
                size,
                MappingTag {
                    flags: mem_flags,
                    region: region.clone(),
                    grant,
                },
            )
        })
        .map_err(allocate_err_to_syscall)?;
    let base = allocated.base();

    if let Err(e) = region.install(user_vm.mapper(), base, mem_flags) {
        user_vm.allocator().with_lock(|alloc| {
            let released = alloc.free(base, size);
            debug_assert!(
                released.is_ok(),
                "rollback of just-allocated region must succeed"
            );
        });
        return Err(map_err_to_syscall(&e));
    }

    Ok(base.as_usize() as u64)
}

/// `memory_remap(va, size, flags) -> ()`
///
/// Меняет флаги уже зарегистрированного маппинга. Регион не меняется,
/// апгрейд ограничен `grant`'ом, сохранённым при `MemoryMap`/`MemoryAllocate`:
/// иначе процесс с handle'ом на R-only смог бы апгрейднуть mapping до RW
/// (сам handle на этапе remap не проверяется - только VA).
pub fn sys_memory_remap(va_raw: u64, size_bytes: u64, flags_raw: u64) -> Result<u64, SyscallError> {
    let size = parse_size(size_bytes)?;
    let va_usize = usize::try_from(va_raw).map_err(|_| SyscallError::InvalidArgument)?;
    let flags = parse_flags(flags_raw)?;
    let mem_flags = flags.to_mem_flags();
    let base =
        PageAlignedVirtualAddress::from_usize(va_usize).ok_or(SyscallError::InvalidArgument)?;

    let user_vm = runtime().current_user_vm().ok_or(SyscallError::WrongType)?;

    let (region, grant) = user_vm
        .allocator()
        .with_lock(|alloc| {
            alloc
                .lookup(base, size)
                .map(|range| (range.tag().region.clone(), range.tag().grant))
        })
        .map_err(region_err_to_syscall)?;

    if !grant.allows(access_mask_for(flags)) {
        return Err(SyscallError::InvalidArgument);
    }

    user_vm
        .mapper()
        .remap(base, size.get(), mem_flags)
        .map_err(|e| remap_err_to_syscall(&e))?;

    user_vm.allocator().with_lock(|alloc| {
        let result = alloc.set_tag(
            base,
            size,
            MappingTag {
                flags: mem_flags,
                region,
                grant,
            },
        );
        debug_assert!(result.is_ok(), "region must remain after successful remap");
    });

    Ok(0)
}

/// `memory_free(va, size) -> ()`
///
/// Снимает маппинг и убирает запись из аллокатора. `Arc<MemoryRegion>` в
/// теге дропается; если последний - `Drop` региона возвращает фреймы в FA.
pub fn sys_memory_free(va_raw: u64, size_bytes: u64) -> Result<u64, SyscallError> {
    let size = parse_size(size_bytes)?;
    let va_usize = usize::try_from(va_raw).map_err(|_| SyscallError::InvalidArgument)?;
    let base =
        PageAlignedVirtualAddress::from_usize(va_usize).ok_or(SyscallError::InvalidArgument)?;

    let user_vm = runtime().current_user_vm().ok_or(SyscallError::WrongType)?;

    user_vm
        .allocator()
        .with_lock(|alloc| alloc.lookup(base, size).map(|_| ()))
        .map_err(region_err_to_syscall)?;

    user_vm
        .mapper()
        .unmap(base, size.get())
        .map_err(|e| unmap_err_to_syscall(&e))?;

    user_vm.allocator().with_lock(|alloc| {
        let result = alloc.free(base, size);
        debug_assert!(result.is_ok(), "region must remain after successful unmap");
    });

    Ok(0)
}

/// `memory_region_inspect(region_handle)`
///
/// Primary возврат - `size_bytes`, secondary - `(kind_tag << 16) | access_bits`.
pub fn sys_memory_region_inspect(frame: &mut dyn SyscallFrame) {
    let result = (|| -> Result<(u64, u64), SyscallError> {
        let id = parse_handle_id(frame.arg(0))?;
        let region = lookup_memory(id, Rights::INSPECT)?;
        let size = region.size_bytes() as u64;
        let secondary =
            (u64::from(region.kind_tag()) << 16) | u64::from(region.access_mask().bits());
        Ok((size, secondary))
    })();

    match result {
        Ok((size, secondary)) => {
            frame.set_secondary_return(secondary);
            frame.set_return(encode_return(Ok(size)));
        }
        Err(e) => frame.set_return(e.into()),
    }
}

fn allocate_err_to_syscall(err: AllocateError) -> SyscallError {
    match err {
        AllocateError::UnalignedSize => SyscallError::InvalidArgument,
        AllocateError::NotEnoughSpace | AllocateError::OutOfSlots => SyscallError::OutOfMemory,
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

fn region_create_err_to_syscall(err: &RegionCreateError) -> SyscallError {
    match err {
        RegionCreateError::OutOfMemory => SyscallError::OutOfMemory,
    }
}

fn region_err_to_syscall(err: RangeError) -> SyscallError {
    match err {
        RangeError::NotFound => SyscallError::NotFound,
        RangeError::UnalignedSize => SyscallError::InvalidArgument,
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

fn unmap_err_to_syscall(err: &MemoryUnmappingError) -> SyscallError {
    match err {
        MemoryUnmappingError::NotMapped => SyscallError::NotFound,
        MemoryUnmappingError::MisalignedRange | MemoryUnmappingError::UnsupportedBlockMapping => {
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
            allocate_err_to_syscall(AllocateError::UnalignedSize),
            SyscallError::InvalidArgument
        );
        assert_eq!(
            allocate_err_to_syscall(AllocateError::NotEnoughSpace),
            SyscallError::OutOfMemory
        );
        assert_eq!(
            allocate_err_to_syscall(AllocateError::OutOfSlots),
            SyscallError::OutOfMemory
        );
    }

    #[test]
    fn region_err_mapping() {
        assert_eq!(
            region_err_to_syscall(RangeError::NotFound),
            SyscallError::NotFound
        );
        assert_eq!(
            region_err_to_syscall(RangeError::UnalignedSize),
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

    #[test]
    fn unmap_err_mapping() {
        assert_eq!(
            unmap_err_to_syscall(&MemoryUnmappingError::NotMapped),
            SyscallError::NotFound
        );
        assert_eq!(
            unmap_err_to_syscall(&MemoryUnmappingError::MisalignedRange),
            SyscallError::InvalidArgument
        );
        assert_eq!(
            unmap_err_to_syscall(&MemoryUnmappingError::UnsupportedBlockMapping),
            SyscallError::InvalidArgument
        );
    }

    #[test]
    fn rights_for_access_combinations() {
        assert_eq!(rights_for_access(UserMemFlags::ReadOnly), Rights::READ);
        assert_eq!(
            rights_for_access(UserMemFlags::ReadWrite),
            Rights::READ | Rights::WRITE
        );
        assert_eq!(
            rights_for_access(UserMemFlags::ReadExecute),
            Rights::READ | Rights::EXECUTE
        );
    }

    #[test]
    fn access_mask_for_combinations() {
        assert_eq!(
            access_mask_for(UserMemFlags::ReadOnly).bits(),
            AccessMask::R.bits()
        );
        assert_eq!(
            access_mask_for(UserMemFlags::ReadWrite).bits(),
            AccessMask::RW.bits()
        );
        assert_eq!(
            access_mask_for(UserMemFlags::ReadExecute).bits(),
            AccessMask::RX.bits()
        );
    }

    #[test]
    fn region_create_err_mapping() {
        assert_eq!(
            region_create_err_to_syscall(&RegionCreateError::OutOfMemory),
            SyscallError::OutOfMemory
        );
    }

    #[test]
    fn parse_access_mask_zero_is_invalid() {
        assert_eq!(parse_access_mask(0), Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn parse_access_mask_truncates_high_bits() {
        // Только нижние 3 бита значимы.
        let mask = parse_access_mask(0xFF).expect("non-zero low bits");
        assert_eq!(mask.bits(), AccessMask::RWX.bits());
    }

    #[test]
    fn parse_access_mask_recognizes_canonical_combinations() {
        assert_eq!(
            parse_access_mask(AccessMask::R.bits() as u64).map(|m| m.bits()),
            Ok(AccessMask::R.bits())
        );
        assert_eq!(
            parse_access_mask(AccessMask::RW.bits() as u64).map(|m| m.bits()),
            Ok(AccessMask::RW.bits())
        );
        assert_eq!(
            parse_access_mask(AccessMask::RX.bits() as u64).map(|m| m.bits()),
            Ok(AccessMask::RX.bits())
        );
    }

    #[test]
    fn page_alignment_is_4k() {
        use memory::aligned::Aligned;
        assert_eq!(PageAlignedVirtualAddress::ALIGNMENT, 4096);
    }
}
