//! Обработчики Process-syscall: create/self/load_image/exit_code/terminate/start.
//!
//! Парсят аргументы, пробрасывают в `capability` API и регистрируют новые
//! handle в текущей handle-таблице вызывающего процесса.

use alloc::{sync::Arc, vec::Vec};
use core::num::NonZeroU32;

use capability::{
    Capability, CapabilityTarget, DrainedHandles, HandleId, HandleReservation, HandleTable,
    LoadImageError, MeteringGrant, ProcessObject, Resource, Rights, StartProcessError,
    UserImageInstall, UserSegmentInstall, UserStartSpec, UserThreadEntry, default_rights_for,
    install_handle, runtime,
};
use collections::{LockCell, MutexCell};
use memory::{
    MemoryRegion, UserVmContext,
    memory_mapper::{MemoryMappingError, UserCopyError},
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use syscall::{
    MAX_PROCESS_NAME_LEN, MAX_SEGMENTS_PER_IMG, SEGMENT_ABI_VERSION, SyscallError,
    USER_IMAGE_DESC_SIZE, USER_SEGMENT_SIZE, UserMemFlags, decode_image_desc, decode_segment,
};

use super::{
    bridge::parse_handle_id,
    error::{map_ipc_error, map_spawn_error},
    flags::to_mem_flags,
    runtime::runtime as syscall_runtime,
};

const EXIT_CODE_START_ROLLBACK: i32 = -1;

pub fn sys_process_create(name_va: u64, name_len: u64) -> Result<u64, SyscallError> {
    let len = usize::try_from(name_len).map_err(|_| SyscallError::InvalidArgument)?;
    if len == 0 || len > MAX_PROCESS_NAME_LEN {
        return Err(SyscallError::InvalidArgument);
    }
    if name_va == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    let mut buf = [0u8; MAX_PROCESS_NAME_LEN];
    let user_vm = syscall_runtime()
        .current_user_vm()
        .ok_or(SyscallError::WrongType)?;
    let target = buf.get_mut(..len).ok_or(SyscallError::InvalidArgument)?;
    copy_in(&user_vm, name_va, target)?;
    let name = core::str::from_utf8(target).map_err(|_| SyscallError::InvalidArgument)?;

    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let reservation = table
        .with_lock(HandleTable::reserve_slot)
        .map_err(map_ipc_error)?;
    let process = match syscall_runtime().create_empty_process(name) {
        Ok(p) => p,
        Err(e) => {
            table.with_lock(|tbl| tbl.release_reservation(reservation));
            return Err(map_spawn_error(e));
        }
    };
    Ok(commit_object_handle(
        &table,
        reservation,
        CapabilityTarget::Process(process),
    ))
}

/// Capability на собственный процесс вызывающего.
pub fn sys_process_self() -> Result<u64, SyscallError> {
    let process = syscall_runtime()
        .current_process_object()
        .ok_or(SyscallError::WrongType)?;
    let rights = default_rights_for(&CapabilityTarget::Process(process.clone()));
    let handle_id =
        install_handle(Capability::new_unheld_process(process, rights)).map_err(map_ipc_error)?;
    Ok(u64::from(handle_id.raw().get()))
}

/// Capability на метеринг-`Resource` текущего процесса. Права капы - те, с которыми ресурс
/// передали при старте. Шире прав передавшего хэндла процессу не достаётся.
/// `WrongType`, если процесс стартовал без метеринг-ресурса.
pub fn sys_process_resource_self() -> Result<u64, SyscallError> {
    let process = syscall_runtime()
        .current_process_object()
        .ok_or(SyscallError::WrongType)?;
    let grant = process.metering().ok_or(SyscallError::WrongType)?;
    let handle_id = install_handle(Capability::new(
        CapabilityTarget::Resource(grant.resource),
        grant.rights,
    ))
    .map_err(map_ipc_error)?;
    Ok(u64::from(handle_id.raw().get()))
}

/// `resource_split(resource_handle, budget_pages) -> resource_handle`
///
/// Выделяет `budget_pages` страниц бюджета в производный `Resource` с тем же
/// потолком доступа. Требует `Rights::WRITE` на списание с исходного ресурса и
/// `Rights::DUPLICATE` на деривацию. Отзыв исходной капы отзывает и производную.
pub fn sys_resource_split(resource_h: u64, budget_pages: u64) -> Result<u64, SyscallError> {
    let resource_id = parse_handle_id(resource_h)?;
    if budget_pages == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let parent = table
        .with_lock(|tbl| tbl.get_object::<Resource>(resource_id, Rights::WRITE))
        .map_err(map_ipc_error)?;
    // Отказ деривации освобождает производный ресурс, и его Drop возвращает
    // списанные страницы исходному.
    let child = parent.split(budget_pages).map_err(map_ipc_error)?;

    let id = table
        .with_lock(|tbl| tbl.derive(resource_id, CapabilityTarget::Resource(child)))
        .map_err(|rejected| map_ipc_error(rejected.release()))?;
    Ok(u64::from(id.raw().get()))
}

pub fn sys_process_exit_code(handle: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let process = table
        .with_lock(|tbl| tbl.get_object::<ProcessObject>(id, Rights::READ))
        .map_err(map_ipc_error)?;
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
        .with_lock(|tbl| tbl.get_object::<ProcessObject>(id, Rights::WRITE))
        .map_err(map_ipc_error)?;

    // Self-terminate отвергается: dispatcher вернул бы в уже завершённый поток.
    // Self-exit - через ThreadExit (0x52).
    if let Some(current) = syscall_runtime().current_process_object()
        && Arc::ptr_eq(&current, &process)
    {
        return Err(SyscallError::AccessDenied);
    }

    syscall_runtime()
        .terminate_process(&process, code)
        .map_err(map_ipc_error)?;
    Ok(0)
}

/// `ProcessLoadImage` - устанавливает сегменты образа в child AS.
/// Требует `Rights::WRITE` на process и корректной геометрии дескриптора.
pub fn sys_process_load_image(
    process_h: u64,
    desc_va: u64,
    desc_len: u64,
) -> Result<u64, SyscallError> {
    let process_id = parse_handle_id(process_h)?;
    if desc_len != USER_IMAGE_DESC_SIZE as u64 {
        return Err(SyscallError::InvalidArgument);
    }

    let user_vm = syscall_runtime()
        .current_user_vm()
        .ok_or(SyscallError::WrongType)?;

    let mut desc_buf = [0u8; USER_IMAGE_DESC_SIZE];
    copy_in(&user_vm, desc_va, &mut desc_buf)?;
    let desc = decode_image_desc(&desc_buf).ok_or(SyscallError::InvalidArgument)?;
    if desc.version != SEGMENT_ABI_VERSION {
        return Err(SyscallError::InvalidArgument);
    }
    let segment_count = desc.segment_count as usize;
    if segment_count == 0 || segment_count > MAX_SEGMENTS_PER_IMG {
        return Err(SyscallError::InvalidArgument);
    }

    let loader_table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let process_object = loader_table
        .with_lock(|tbl| tbl.get_object::<ProcessObject>(process_id, Rights::WRITE))
        .map_err(map_ipc_error)?;

    let mut segments_buf = [0u8; MAX_SEGMENTS_PER_IMG * USER_SEGMENT_SIZE];
    let total = segment_count * USER_SEGMENT_SIZE;
    let staging = segments_buf
        .get_mut(..total)
        .ok_or(SyscallError::InvalidArgument)?;
    copy_in(&user_vm, desc.segments_va, staging)?;

    let mut segments: Vec<UserSegmentInstall> = Vec::with_capacity(segment_count);
    let (chunks, _) = staging.as_chunks::<USER_SEGMENT_SIZE>();
    for chunk in chunks {
        let seg = decode_segment(chunk).ok_or(SyscallError::InvalidArgument)?;
        if seg.reserved != 0 {
            return Err(SyscallError::InvalidArgument);
        }
        let flags = UserMemFlags::from_raw(u64::from(seg.flags))
            .map_err(|_| SyscallError::InvalidArgument)?;
        let needed_rights = Rights::WRITE | rights_for_access(flags);
        let region_handle_id = handle_id_from_raw(seg.region_handle)?;
        let region = loader_table
            .with_lock(|tbl| tbl.get_object::<MemoryRegion>(region_handle_id, needed_rights))
            .map_err(map_ipc_error)?;
        if u64::try_from(region.size_bytes()).is_ok_and(|size| size != seg.mapped_size) {
            return Err(SyscallError::InvalidArgument);
        }
        if !region.access_mask().allows(access_mask_for(flags)) {
            return Err(SyscallError::AccessDenied);
        }
        let va_base = usize::try_from(seg.va_base)
            .ok()
            .and_then(PageAlignedVirtualAddress::from_usize)
            .ok_or(SyscallError::InvalidArgument)?;
        segments.push(UserSegmentInstall {
            va_base,
            mapped_size: usize::try_from(seg.mapped_size)
                .map_err(|_| SyscallError::InvalidArgument)?,
            region,
            flags: to_mem_flags(flags),
        });
    }

    let user_vm_base = usize::try_from(desc.user_vm_base)
        .ok()
        .and_then(PageAlignedVirtualAddress::from_usize)
        .ok_or(SyscallError::InvalidArgument)?;
    let entry = usize::try_from(desc.entry_va).map_err(|_| SyscallError::InvalidArgument)?;
    let user_vm_size =
        usize::try_from(desc.user_vm_size).map_err(|_| SyscallError::InvalidArgument)?;
    let install = UserImageInstall {
        segments,
        entry: VirtualAddress::new(entry),
        user_vm_base,
        user_vm_size,
    };
    syscall_runtime()
        .load_user_image_into(&process_object, &install)
        .map_err(load_image_err_to_syscall)?;
    Ok(0)
}

/// `ProcessStart` - стартует первый поток процесса.
/// ABI: `bootstrap_handle` - raw HandleId стартового хэндла-канала. `priority` в битах [0..8),
/// биты [8..) = 0.
pub fn sys_process_start(
    process_h: u64,
    entry_pc: u64,
    user_sp: u64,
    bootstrap_handle: u64,
    priority: u64,
    resource_handle: u64,
) -> Result<u64, SyscallError> {
    let process_id = parse_handle_id(process_h)?;
    if priority & !0xFF != 0 {
        return Err(SyscallError::InvalidArgument);
    }
    let priority = (priority & 0xFF) as u8;
    let handle_raw = u32::try_from(bootstrap_handle).map_err(|_| SyscallError::InvalidArgument)?;
    let nz = NonZeroU32::new(handle_raw).ok_or(SyscallError::InvalidArgument)?;
    let handle_id = HandleId::from_raw(nz);

    let loader_table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let metering_grant = if resource_handle == 0 {
        // Наследование: потомку достаётся грант родителя с теми же правами.
        syscall_runtime()
            .current_process_object()
            .and_then(|caller| caller.metering())
    } else {
        let resource_id = parse_handle_id(resource_handle)?;
        // Гейт тот же, что у `ResourceSplit`: отдача ресурса - деривация капы в чужую таблицу.
        // Права гранта - права этого хэндла: `ProcessResourceSelf` у потомка не выдаст шире,
        // и намеренно неделегируемая капа (без `TRANSFER`) не станет делегируемой через старт.
        let access = loader_table
            .with_lock(|tbl| {
                tbl.get_object_with_rights::<Resource>(
                    resource_id,
                    Rights::WRITE | Rights::DUPLICATE,
                )
            })
            .map_err(map_ipc_error)?;
        Some(MeteringGrant {
            resource: access.object,
            rights: access.rights,
        })
    };
    let process_object = loader_table
        .with_lock(|tbl| tbl.get_object::<ProcessObject>(process_id, Rights::WRITE))
        .map_err(map_ipc_error)?;

    let spec = UserStartSpec {
        entry: UserThreadEntry {
            entry_pc,
            user_sp,
            // X0 потомка вычисляет планировщик из child-table id хэндла-канала.
            arg: 0,
            priority,
        },
        loader_handle_table: loader_table.clone(),
        handle_id,
        metering_grant,
    };

    let thread = syscall_runtime()
        .start_user_process(&process_object, spec)
        .map_err(|e| start_err_to_syscall(&e))?;

    // Предрезерв слота отказывал бы на штатном пути: caller-table бывает полна, и
    // освобождает её как раз drain переданного bootstrap-хэндла. Но освобождённый
    // слот может и не переиспользоваться - на MAX_GENERATION pop_free_slot выводит
    // его из оборота. Тогда потомок уже запущен, а на него нет хэндла, и старт
    // откатывается терминацией.
    let insert_result = loader_table.with_lock(|tbl| {
        tbl.insert(Capability::new_with_default_rights(
            CapabilityTarget::Thread(thread.clone()),
        ))
    });
    let handle_id = match insert_result {
        Ok(handle_id) => handle_id,
        Err(rejected) => {
            let _ = syscall_runtime().terminate_process(&process_object, EXIT_CODE_START_ROLLBACK);
            return Err(map_ipc_error(rejected.release()));
        }
    };

    Ok(u64::from(handle_id.raw().get()))
}

pub(super) fn install_object_handle(target: CapabilityTarget) -> Result<u64, SyscallError> {
    let handle_id =
        install_handle(Capability::new_with_default_rights(target)).map_err(map_ipc_error)?;
    Ok(u64::from(handle_id.raw().get()))
}

pub(super) fn commit_object_handle(
    table: &Arc<MutexCell<HandleTable>>,
    reservation: HandleReservation,
    target: CapabilityTarget,
) -> u64 {
    let mut rejected = DrainedHandles::empty();
    let handle_id = table.with_lock(|tbl| {
        tbl.commit_reserved(
            reservation,
            Capability::new_with_default_rights(target),
            &mut rejected,
        )
    });
    // Отвергнутое разрушается вне table-лока.
    rejected.release();
    u64::from(handle_id.raw().get())
}

fn handle_id_from_raw(raw: u32) -> Result<HandleId, SyscallError> {
    let nz = NonZeroU32::new(raw).ok_or(SyscallError::InvalidArgument)?;
    Ok(HandleId::from_raw(nz))
}

fn rights_for_access(flags: UserMemFlags) -> Rights {
    match flags {
        UserMemFlags::ReadOnly => Rights::READ,
        UserMemFlags::ReadWrite => Rights::READ | Rights::WRITE,
        UserMemFlags::ReadExecute => Rights::READ | Rights::EXECUTE,
    }
}

fn access_mask_for(flags: UserMemFlags) -> memory::AccessMask {
    match flags {
        UserMemFlags::ReadOnly => memory::AccessMask::R,
        UserMemFlags::ReadWrite => memory::AccessMask::RW,
        UserMemFlags::ReadExecute => memory::AccessMask::RX,
    }
}

fn load_image_err_to_syscall(e: LoadImageError) -> SyscallError {
    match e {
        LoadImageError::ProcessNotFound => SyscallError::BadHandle,
        LoadImageError::WrongState | LoadImageError::NoUserAddressSpace => SyscallError::WrongType,
        LoadImageError::UserVmRangeOverflow | LoadImageError::InvalidGeometry => {
            SyscallError::InvalidArgument
        }
        LoadImageError::MappingFailed(m) => mapping_err_to_syscall(&m),
    }
}

fn mapping_err_to_syscall(e: &MemoryMappingError) -> SyscallError {
    match e {
        MemoryMappingError::OutOfMemory => SyscallError::OutOfMemory,
        MemoryMappingError::AlreadyMapped
        | MemoryMappingError::VirtualMappingError
        | MemoryMappingError::InitExceedsMapping => SyscallError::InvalidArgument,
    }
}

fn start_err_to_syscall(err: &StartProcessError) -> SyscallError {
    match err {
        StartProcessError::ProcessNotFound => SyscallError::BadHandle,
        StartProcessError::WrongState => SyscallError::WrongType,
        StartProcessError::HandleValidationFailed(ipc) => map_ipc_error(*ipc),
        StartProcessError::SpawnFailed(source) => map_spawn_error(*source),
    }
}

pub(super) fn exit_code_from_arg(raw: u64) -> i32 {
    #[allow(clippy::cast_possible_truncation)]
    let code = raw as u32;
    code.cast_signed()
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
    use syscall::{MAX_SEGMENTS_PER_IMG, USER_IMAGE_DESC_SIZE};

    use super::*;

    #[test]
    fn sys_resource_split_rejects_zero_budget() {
        assert_eq!(sys_resource_split(1, 0), Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn sys_resource_split_rejects_zero_handle() {
        assert_eq!(sys_resource_split(0, 4), Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn sys_process_load_image_rejects_bad_desc_len() {
        assert_eq!(
            sys_process_load_image(1, 0x1000, 0),
            Err(SyscallError::InvalidArgument)
        );
        assert_eq!(
            sys_process_load_image(1, 0x1000, (USER_IMAGE_DESC_SIZE - 1) as u64),
            Err(SyscallError::InvalidArgument)
        );
        assert_eq!(
            sys_process_load_image(1, 0x1000, (USER_IMAGE_DESC_SIZE + 1) as u64),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn sys_process_load_image_rejects_zero_process_handle() {
        assert_eq!(
            sys_process_load_image(0, 0x1000, USER_IMAGE_DESC_SIZE as u64),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn sys_process_start_rejects_zero_handle() {
        // process_h валиден (parse), bootstrap_handle (arg3) = 0 - невалиден.
        assert_eq!(
            sys_process_start(1, 0x4000_0000, 0x4001_0000, 0, 0, 0),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn sys_process_start_rejects_reserved_bits_in_priority() {
        let bad_priority = 1u64 << 8;
        assert_eq!(
            sys_process_start(1, 0x4000_0000, 0x4001_0000, 1, bad_priority, 0),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn abi_limits_are_stable() {
        assert_eq!(MAX_SEGMENTS_PER_IMG, 16);
    }

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
        assert_eq!(exit_code_from_arg(0xFFFF_FFFF), -1);
        assert_eq!(exit_code_from_arg(0x1_0000_0000 | 7), 7);
    }
}
