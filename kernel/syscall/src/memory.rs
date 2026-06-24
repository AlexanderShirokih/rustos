//! Handler-ы memory-syscall'ов.
//!
//! ABI флагов памяти: 0 = RW, 1 = RO, 2 = RX.

use alloc::sync::Arc;
use core::num::NonZeroUsize;

use collections::LockCell;
use kobject::{Handle, IpcError, KObject, ResourceBudgetRefund, RevocationHook, Rights};
use memory::{
    AccessMask, MappingTag, MemoryRegion, RegionCreateError, UserVmContext, WeakUserVmContext,
    memory_mapper::{MemoryMappingError, MemoryRemappingError},
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

// Срыв активного маппинга при отзыве авторизовавшей его капы. Держит СЛАБЫЙ
// снимок AS получателя: сильную ссылку на сам MemoryMapping хранит
// MappingTag::revocation, и weak-ссылка на AS разрывает цикл
// MappingTag -> MemoryMapping -> AS -> MappingTag (иначе AS течёт после
// transfer'а капы, когда HandleTable::Drop процесса её уже не отзывает).
struct MemoryMapping {
    user_vm: WeakUserVmContext,
    base: PageAlignedVirtualAddress,
    size: NonZeroUsize,
}

impl RevocationHook for MemoryMapping {
    fn revoke(&self) {
        // None -> AS уже разрушено (процесс завершился), срывать нечего.
        if let Some(user_vm) = self.user_vm.upgrade() {
            user_vm.unmap_range(self.base, self.size);
        }
    }
}

fn parse_flags(raw: u64) -> Result<UserMemFlags, SyscallError> {
    UserMemFlags::from_raw(raw).map_err(|_| SyscallError::InvalidArgument)
}

fn parse_size(size_bytes: u64) -> Result<NonZeroUsize, SyscallError> {
    let size = usize::try_from(size_bytes).map_err(|_| SyscallError::InvalidArgument)?;
    NonZeroUsize::new(size).ok_or(SyscallError::InvalidArgument)
}

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

/// `memory_create_virtual(resource_handle, size_bytes, access_mask) -> region_handle`
///
/// Требует `Rights::WRITE` на ресурс; бюджет возвращается при дропе региона.
pub fn sys_memory_create_virtual(
    resource_h: u64,
    size_bytes: u64,
    access_raw: u64,
) -> Result<u64, SyscallError> {
    let resource_id = parse_handle_id(resource_h)?;
    let size = parse_size(size_bytes)?;
    let access = parse_access_mask(access_raw)?;
    let pages = NonZeroUsize::new(size.get() / PAGE_SIZE).ok_or(SyscallError::InvalidArgument)?;
    if pages.get() * PAGE_SIZE != size.get() {
        return Err(SyscallError::InvalidArgument);
    }

    let table = kobject::runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let resource = table.with_lock(|tbl| tbl.get_resource(resource_id, Rights::WRITE))?;

    let fa = runtime()
        .frame_allocator()
        .ok_or(SyscallError::OutOfMemory)?;
    let region = MemoryRegion::create_virtual(fa, pages, access)
        .map_err(|e| region_create_err_to_syscall(&e))?;
    // Метерим после выделения: на нехватке бюджета регион дропается (фреймы
    // возвращаются в FA), списания нет. Списать раньше нельзя - refund живёт
    // только на регионе, а до его создания утёк бы при OOM фреймов.
    resource.try_consume(pages.get() as u64)?;
    let region = region.with_refund(ResourceBudgetRefund::new(&resource, pages.get() as u64));
    let region_arc = Arc::new(region);
    let ko = KObject::Memory(region_arc);
    let handle = Handle::new(ko.clone(), Rights::defaults_for(&ko));
    let id = table.with_lock(|tbl| tbl.insert(handle))?;
    Ok(u64::from(id.raw().get()))
}

/// `memory_create_physical(resource_handle, pa, size_bytes, access_mask) -> region_handle`
///
/// PA должен быть page-aligned и лежать в пределах ресурса; маска доступа не
/// превышает маску ресурса. Бюджет (div_ceil по страницам) возвращается при дропе.
pub fn sys_memory_create_physical(
    resource_h: u64,
    pa_raw: u64,
    size_bytes: u64,
    access_raw: u64,
) -> Result<u64, SyscallError> {
    let resource_id = parse_handle_id(resource_h)?;
    let size = parse_size(size_bytes)?;
    let access = parse_access_mask(access_raw)?;
    let pa_usize = usize::try_from(pa_raw).map_err(|_| SyscallError::InvalidArgument)?;
    let pa = PageAlignedAddress::from_usize(pa_usize).ok_or(SyscallError::InvalidArgument)?;

    let table = kobject::runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let resource = table.with_lock(|tbl| tbl.get_resource(resource_id, Rights::WRITE))?;
    if !resource.permits(pa, size, access) {
        return Err(SyscallError::AccessDenied);
    }

    // Округление вверх: physical-окно может быть не кратно странице.
    let pages = size.get().div_ceil(PAGE_SIZE) as u64;
    resource.try_consume(pages)?;

    // refund на регионе вернёт бюджет на дропе - в т.ч. если insert ниже упадёт.
    let refund = ResourceBudgetRefund::new(&resource, pages);
    let region = MemoryRegion::create_physical(pa, size, access).with_refund(refund);
    let region_arc = Arc::new(region);
    let ko = KObject::Memory(region_arc);
    let handle = Handle::new(ko.clone(), Rights::defaults_for(&ko));
    let id = table.with_lock(|tbl| tbl.insert(handle))?;
    Ok(u64::from(id.raw().get()))
}

/// `memory_allocate(resource_handle, size, flags) -> va`
///
/// Анонимный маппинг без handle; бюджет возвращается при `memory_free`/смерти AS.
pub fn sys_memory_allocate(
    resource_h: u64,
    size_bytes: u64,
    flags_raw: u64,
) -> Result<u64, SyscallError> {
    let resource_id = parse_handle_id(resource_h)?;
    let size = parse_size(size_bytes)?;
    let flags = parse_flags(flags_raw)?;
    let mem_flags = flags.to_mem_flags();
    let access = access_mask_for(flags);

    let table = kobject::runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let resource = table.with_lock(|tbl| tbl.get_resource(resource_id, Rights::WRITE))?;

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
    // Метерим после выделения: на нехватке бюджета регион дропается (фреймы
    // возвращаются в FA), списания нет.
    resource.try_consume(pages_count.get() as u64)?;
    let region = Arc::new(region.with_refund(ResourceBudgetRefund::new(
        &resource,
        pages_count.get() as u64,
    )));

    // Fastpath без handle: caller владеет полным access_mask нового региона.
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
                    // Анонимный fastpath без публичной капы: отзывать нечего,
                    // маппинг снимается только при memory_free/смерти AS.
                    revocation: None,
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
pub fn sys_memory_map(
    handle_raw: u64,
    size_bytes: u64,
    flags_raw: u64,
) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle_raw)?;
    let size = parse_size(size_bytes)?;
    let flags = parse_flags(flags_raw)?;
    let mem_flags = flags.to_mem_flags();
    let need = rights_for_access(flags);

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
                    // Заполняется ниже, когда известен base.
                    revocation: None,
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

    // Привязываем отзыв: маппинг живёт ровно столько, сколько авторизовавшая
    // его капа. Если капа отозвана в гонке после lookup, регистрация падает -
    // тогда маппинг снимается и syscall возвращает ошибку (а не живой VA на
    // отозванную капу).
    register_mapping_revocation(
        &user_vm,
        base,
        size,
        id,
        MappingTag {
            flags: mem_flags,
            region,
            grant,
            revocation: None,
        },
    )?;

    Ok(base.as_usize() as u64)
}

// Привязывает срыв к установленному маппингу. Если капа отозвана в гонке до
// регистрации - маппинг срывается здесь и syscall валится (иначе живой VA на
// отозванную капу).
fn register_mapping_revocation(
    user_vm: &UserVmContext,
    base: PageAlignedVirtualAddress,
    size: NonZeroUsize,
    cap_id: kobject::HandleId,
    mut tag: MappingTag,
) -> Result<(), SyscallError> {
    let mapping = Arc::new(MemoryMapping {
        user_vm: user_vm.downgrade(),
        base,
        size,
    });
    let hook: Arc<dyn RevocationHook> = mapping.clone();
    let weak = Arc::downgrade(&hook);
    tag.revocation = Some(mapping as Arc<dyn core::any::Any + Send + Sync>);

    user_vm.allocator().with_lock(|alloc| {
        let result = alloc.set_tag(base, size, tag);
        debug_assert!(result.is_ok(), "range must exist right after install");
    });

    // Порядок локов: чужой UserVmContext (выше) уже отпущен, теперь HandleTable.
    let registered = kobject::runtime()
        .current_handle_table()
        .map_or(Err(IpcError::BadHandle), |table| {
            table.with_lock(|tbl| tbl.register_revocation_hook(cap_id, weak))
        });

    if registered.is_err() {
        user_vm.unmap_range(base, size);
        return Err(SyscallError::Revoked);
    }
    Ok(())
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

    let (region, grant, revocation) = user_vm
        .allocator()
        .with_lock(|alloc| {
            alloc.lookup(base, size).map(|range| {
                (
                    range.tag().region.clone(),
                    range.tag().grant,
                    // Сохраняем keep-alive отзыва: remap меняет только флаги,
                    // маппинг (а значит и его отзываемость) остаётся тем же.
                    range.tag().revocation.clone(),
                )
            })
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
                revocation,
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

    // Явная валидация: free на не-выделенный диапазон обязан вернуть NotFound
    // (в отличие от идемпотентного отзыва, который такой случай глотает).
    user_vm
        .allocator()
        .with_lock(|alloc| alloc.lookup(base, size).map(|_| ()))
        .map_err(region_err_to_syscall)?;

    user_vm.unmap_range(base, size);

    Ok(0)
}

/// `memory_region_inspect(region_handle)`
///
/// Primary возврат - `size_bytes`, secondary - `(kind_tag << 16) | access_bits`.
pub fn sys_memory_region_inspect(frame: &mut dyn SyscallFrame) {
    let result = (|| -> Result<(u64, u64), SyscallError> {
        let id = parse_handle_id(frame.arg(0))?;
        let region = lookup_memory(id, Rights::READ)?;
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
        let mask = parse_access_mask(0xFF).expect("non-zero low bits");
        assert_eq!(mask.bits(), AccessMask::RWX.bits());
    }

    #[test]
    fn parse_access_mask_recognizes_canonical_combinations() {
        assert_eq!(
            parse_access_mask(u64::from(AccessMask::R.bits())).map(AccessMask::bits),
            Ok(AccessMask::R.bits())
        );
        assert_eq!(
            parse_access_mask(u64::from(AccessMask::RW.bits())).map(AccessMask::bits),
            Ok(AccessMask::RW.bits())
        );
        assert_eq!(
            parse_access_mask(u64::from(AccessMask::RX.bits())).map(AccessMask::bits),
            Ok(AccessMask::RX.bits())
        );
    }

    mod revoke {
        use alloc::{sync::Weak, vec::Vec};

        use collections::MutexCell;
        use kobject::{Handle, HandleTable, KObject, RevocationHook};
        use memory::{
            MemFlags, MemoryRegion,
            memory_mapper::{
                AddressSpaceHandle, AddressSpaceTag, MemoryMapper, MemoryMappingError,
                MemoryRemappingError, MemoryUnmappingError,
            },
            physical_address::PhysicalAddress,
            user_vm_allocator::UserVmAllocator,
            virtual_address::VirtualAddress,
        };

        use super::*;

        const ARENA: usize = 0x4000_0000;

        struct TrackingMapper {
            unmapped: MutexCell<Vec<usize>>,
        }

        impl TrackingMapper {
            fn new() -> Self {
                Self {
                    unmapped: MutexCell::new(Vec::new()),
                }
            }
        }

        impl MemoryMapper for TrackingMapper {
            fn map(
                &self,
                _v: PageAlignedVirtualAddress,
                _pc: usize,
                _i: &[u8],
                _f: MemFlags,
            ) -> Result<(), MemoryMappingError> {
                Ok(())
            }
            fn map_exact(
                &self,
                _v: PageAlignedVirtualAddress,
                _p: PageAlignedAddress,
                _s: usize,
                _f: MemFlags,
            ) -> Result<(), MemoryMappingError> {
                Ok(())
            }
            fn unmap(
                &self,
                v: PageAlignedVirtualAddress,
                _s: usize,
            ) -> Result<(), MemoryUnmappingError> {
                self.unmapped.with_lock(|u| u.push(v.as_usize()));
                Ok(())
            }
            fn remap(
                &self,
                _v: PageAlignedVirtualAddress,
                _s: usize,
                _f: MemFlags,
            ) -> Result<(), MemoryRemappingError> {
                Ok(())
            }
            fn activate_handle(&self) -> AddressSpaceHandle {
                AddressSpaceHandle::new(PhysicalAddress::new(0), AddressSpaceTag::NONE)
            }
            fn zero_owned_frame(&self, _p: PageAlignedAddress) {}
            fn as_any(&self) -> &(dyn core::any::Any + 'static) {
                self
            }
        }

        fn nz(v: usize) -> NonZeroUsize {
            NonZeroUsize::new(v).unwrap()
        }

        fn region() -> Arc<MemoryRegion> {
            Arc::new(MemoryRegion::create_physical(
                PageAlignedAddress::from_usize(0x8000_0000).unwrap(),
                nz(PAGE_SIZE),
                AccessMask::RW,
            ))
        }

        fn recipient_with_mapping() -> (
            UserVmContext,
            Arc<TrackingMapper>,
            PageAlignedVirtualAddress,
        ) {
            let mapper = Arc::new(TrackingMapper::new());
            let mut alloc = UserVmAllocator::new(
                PageAlignedVirtualAddress::from_usize(ARENA).unwrap(),
                VirtualAddress::new(ARENA + 16 * PAGE_SIZE),
            );
            let allocated = alloc
                .allocate(
                    nz(PAGE_SIZE),
                    MappingTag {
                        flags: MemFlags::user_rw(),
                        region: region(),
                        grant: AccessMask::RW,
                        revocation: None,
                    },
                )
                .unwrap();
            let base = allocated.base();
            let ctx = UserVmContext::new(mapper.clone(), Arc::new(MutexCell::new(alloc)));
            (ctx, mapper, base)
        }

        #[test]
        fn closing_grantor_cap_unmaps_recipient_mapping() {
            let (recipient_vm, mapper, base) = recipient_with_mapping();

            let mapping = Arc::new(MemoryMapping {
                user_vm: recipient_vm.downgrade(),
                base,
                size: nz(PAGE_SIZE),
            });
            let hook: Arc<dyn RevocationHook> = mapping.clone();
            let weak: Weak<dyn RevocationHook> = Arc::downgrade(&hook);

            let mut grantor = HandleTable::new();
            let mut recipient_tbl = HandleTable::new();
            let rights = Rights::DUPLICATE | Rights::READ | Rights::TRANSFER;
            let root = grantor
                .insert(Handle::new(KObject::Memory(region()), rights))
                .unwrap();
            let derived = grantor.duplicate(root, rights, 0).unwrap();
            let res = recipient_tbl.reserve_slot().unwrap();
            let drained = grantor
                .try_drain_for_transfer(&[derived], Rights::TRANSFER)
                .unwrap();
            let recv_id = recipient_tbl.commit_reserved(res, drained.into_iter().next().unwrap());
            recipient_tbl
                .register_revocation_hook(recv_id, weak)
                .unwrap();

            assert!(mapper.unmapped.with_lock(|u| u.is_empty()));

            grantor.remove(root).unwrap();

            assert_eq!(mapper.unmapped.with_lock(|u| u.clone()), [base.as_usize()]);
        }

        #[test]
        fn dropped_keepalive_makes_revocation_noop() {
            let (recipient_vm, mapper, base) = recipient_with_mapping();
            let mapping = Arc::new(MemoryMapping {
                user_vm: recipient_vm.downgrade(),
                base,
                size: nz(PAGE_SIZE),
            });
            let hook: Arc<dyn RevocationHook> = mapping.clone();
            let weak: Weak<dyn RevocationHook> = Arc::downgrade(&hook);

            let mut table = HandleTable::new();
            let rights = Rights::DUPLICATE | Rights::READ;
            let id = table
                .insert(Handle::new(KObject::Memory(region()), rights))
                .unwrap();
            table.register_revocation_hook(id, weak).unwrap();

            drop(hook);
            drop(mapping);

            table.remove(id).unwrap();
            assert!(mapper.unmapped.with_lock(|u| u.is_empty()));
        }

        #[test]
        fn keepalive_weak_does_not_retain_address_space() {
            let (recipient_vm, _mapper, base) = recipient_with_mapping();
            let alloc_weak = Arc::downgrade(recipient_vm.allocator());
            let mapping = Arc::new(MemoryMapping {
                user_vm: recipient_vm.downgrade(),
                base,
                size: nz(PAGE_SIZE),
            });

            drop(recipient_vm);
            assert!(
                alloc_weak.upgrade().is_none(),
                "AS must not leak through mapping keep-alive"
            );

            let hook: Arc<dyn RevocationHook> = mapping;
            hook.revoke();
        }
    }
}
