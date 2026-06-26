//! Handler-ы Process-syscall'ов: create/self/load_image/exit_code/terminate/start.
//!
//! Парсят аргументы, пробрасывают в `capability` API и регистрируют новые
//! handle'ы в текущей handle-таблице вызывающего процесса.

use alloc::{sync::Arc, vec::Vec};
use core::num::NonZeroU32;

use capability::{
    Capability, CapabilityTarget, HandleId, HandleReservation, HandleTable, LoadImageError, Rights,
    StartProcessError, UserImageInstall, UserSegmentInstall, UserStartSpec, UserThreadEntry,
    install_handle, runtime,
};
use collections::{LockCell, MutexCell};
use memory::{
    UserVmContext,
    memory_mapper::{MemoryMappingError, UserCopyError},
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use syscall::{SyscallError, UserMemFlags};

use super::{
    bridge::parse_handle_id,
    error::{map_ipc_error, map_spawn_error},
    flags::to_mem_flags,
    runtime::runtime as syscall_runtime,
    spawn_abi::{
        MAX_BOOTSTRAP_HANDLES, MAX_SEGMENTS_PER_IMG, SEGMENT_ABI_VERSION, USER_IMAGE_DESC_SIZE,
        USER_SEGMENT_SIZE, decode_image_desc, decode_segment,
    },
};

/// Максимальная длина имени процесса, передаваемая через user-память.
/// В ABI считается в байтах; строка обязана быть UTF-8.
const MAX_PROCESS_NAME_LEN: usize = 64;

pub fn sys_process_create(name_va: u64, name_len: u64) -> Result<u64, SyscallError> {
    let len = usize::try_from(name_len).map_err(|_| SyscallError::InvalidArgument)?;
    if len > MAX_PROCESS_NAME_LEN {
        return Err(SyscallError::InvalidArgument);
    }
    let mut buf = [0u8; MAX_PROCESS_NAME_LEN];
    let name = if len == 0 {
        ""
    } else {
        if name_va == 0 {
            return Err(SyscallError::InvalidArgument);
        }
        let user_vm = syscall_runtime()
            .current_user_vm()
            .ok_or(SyscallError::WrongType)?;
        copy_in(&user_vm, name_va, &mut buf[..len])?;
        core::str::from_utf8(&buf[..len]).map_err(|_| SyscallError::InvalidArgument)?
    };

    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let reservation = table
        .with_lock(capability::HandleTable::reserve_slot)
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

pub fn sys_process_self() -> Result<u64, SyscallError> {
    let process = syscall_runtime()
        .current_process_object()
        .ok_or(SyscallError::WrongType)?;
    install_object_handle(CapabilityTarget::Process(process))
}

/// Capability на метеринг-`Resource` текущего процесса (права включают `WRITE`).
/// `WrongType`, если процесс стартовал без метеринг-ресурса.
pub fn sys_process_resource_self() -> Result<u64, SyscallError> {
    let process = syscall_runtime()
        .current_process_object()
        .ok_or(SyscallError::WrongType)?;
    let resource = process.metering_resource().ok_or(SyscallError::WrongType)?;
    install_object_handle(CapabilityTarget::Resource(resource))
}

pub fn sys_process_exit_code(handle: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let process = table
        .with_lock(|tbl| tbl.get_process(id, Rights::READ))
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
        .with_lock(|tbl| tbl.get_process(id, Rights::WRITE))
        .map_err(map_ipc_error)?;

    // Self-terminate отвергается: dispatcher вернул бы в уже завершённый поток.
    // Self-exit - через ThreadExit (0x52).
    if let Some(current) = syscall_runtime().current_process_object()
        && alloc::sync::Arc::ptr_eq(&current, &process)
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
    if desc_len as usize != USER_IMAGE_DESC_SIZE {
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
        .with_lock(|tbl| tbl.get_process(process_id, Rights::WRITE))
        .map_err(map_ipc_error)?;

    let mut segments_buf = [0u8; MAX_SEGMENTS_PER_IMG * USER_SEGMENT_SIZE];
    let total = segment_count * USER_SEGMENT_SIZE;
    copy_in(&user_vm, desc.segments_va, &mut segments_buf[..total])?;

    let mut segments: Vec<UserSegmentInstall> = Vec::with_capacity(segment_count);
    for i in 0..segment_count {
        let chunk = &segments_buf[i * USER_SEGMENT_SIZE..(i + 1) * USER_SEGMENT_SIZE];
        let seg = decode_segment(chunk).ok_or(SyscallError::InvalidArgument)?;
        if seg.reserved != 0 {
            return Err(SyscallError::InvalidArgument);
        }
        let flags = UserMemFlags::from_raw(u64::from(seg.flags))
            .map_err(|_| SyscallError::InvalidArgument)?;
        let needed_rights = Rights::WRITE | rights_for_access(flags);
        let region_handle_id = handle_id_from_raw(seg.region_handle)?;
        let (region, _handle_rights) = loader_table
            .with_lock(|tbl| tbl.get_memory_with_rights(region_handle_id, needed_rights))
            .map_err(map_ipc_error)?;
        if region.size_bytes() != seg.mapped_size as usize {
            return Err(SyscallError::InvalidArgument);
        }
        if !region.access_mask().allows(access_mask_for(flags)) {
            return Err(SyscallError::AccessDenied);
        }
        let va_base = PageAlignedVirtualAddress::from_usize(seg.va_base as usize)
            .ok_or(SyscallError::InvalidArgument)?;
        segments.push(UserSegmentInstall {
            va_base,
            mapped_size: seg.mapped_size as usize,
            region,
            flags: to_mem_flags(flags),
        });
    }

    let user_vm_base = PageAlignedVirtualAddress::from_usize(desc.user_vm_base as usize)
        .ok_or(SyscallError::InvalidArgument)?;
    let install = UserImageInstall {
        segments,
        entry: VirtualAddress::new(desc.entry_va as usize),
        user_stack_top: VirtualAddress::new(desc.user_stack_top as usize),
        user_stack_size: desc.user_stack_size as usize,
        user_vm_base,
        user_vm_size: desc.user_vm_size as usize,
    };
    syscall_runtime()
        .load_user_image_into(&process_object, &install)
        .map_err(load_image_err_to_syscall)?;
    Ok(0)
}

/// `ProcessStart` - стартует первый поток процесса.
/// ABI: `prio_and_count = priority[0..8] | handles_count[32..64]`; биты [8..32) = 0.
pub fn sys_process_start(
    process_h: u64,
    entry_pc: u64,
    user_sp: u64,
    arg: u64,
    prio_and_count: u64,
    handles_va: u64,
) -> Result<u64, SyscallError> {
    let process_id = parse_handle_id(process_h)?;
    let priority = (prio_and_count & 0xFF) as u8;
    if (prio_and_count >> 8) & 0xFF_FFFF != 0 {
        return Err(SyscallError::InvalidArgument);
    }
    let handles_count = ((prio_and_count >> 32) & 0xFFFF_FFFF) as usize;
    if handles_count > MAX_BOOTSTRAP_HANDLES {
        return Err(SyscallError::InvalidArgument);
    }

    let metering_resource = syscall_runtime()
        .current_process_object()
        .and_then(|caller| caller.metering_resource());

    let loader_table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let process_object = loader_table
        .with_lock(|tbl| tbl.get_process(process_id, Rights::WRITE))
        .map_err(map_ipc_error)?;

    let mut ids: Vec<HandleId> = Vec::with_capacity(handles_count);
    if handles_count > 0 {
        let user_vm = syscall_runtime()
            .current_user_vm()
            .ok_or(SyscallError::WrongType)?;
        let mut buf = [0u8; MAX_BOOTSTRAP_HANDLES * 4];
        copy_in(&user_vm, handles_va, &mut buf[..handles_count * 4])?;
        for i in 0..handles_count {
            let raw = u32::from_le_bytes(
                buf[i * 4..(i + 1) * 4]
                    .try_into()
                    .expect("4 bytes per handle id"),
            );
            let nz = NonZeroU32::new(raw).ok_or(SyscallError::InvalidArgument)?;
            ids.push(HandleId::from_raw(nz));
        }
    }

    let spec = UserStartSpec {
        entry: UserThreadEntry {
            entry_pc,
            user_sp,
            arg,
            priority,
        },
        loader_handle_table: loader_table.clone(),
        handle_ids: ids,
        metering_resource,
    };

    // Пре-резерв нужен только когда drain не освободит слот в caller-table.
    // При `handles_count > 0` drain гарантированно отдаст >= 1 слот, и
    // пост-drain `reserve_slot` не упадёт. Это спасает легитимные старты с
    // ровно заполненной caller-table, где пере-передача bootstrap-handle-а
    // фактически освобождает слот под возвращаемый thread-handle.
    let pre_reservation = if handles_count == 0 {
        Some(
            loader_table
                .with_lock(capability::HandleTable::reserve_slot)
                .map_err(map_ipc_error)?,
        )
    } else {
        None
    };

    let thread = match syscall_runtime().start_user_process(&process_object, spec) {
        Ok(t) => t,
        Err(e) => {
            if let Some(r) = pre_reservation {
                loader_table.with_lock(|tbl| tbl.release_reservation(r));
            }
            return Err(start_err_to_syscall(&e));
        }
    };

    let reservation = match pre_reservation {
        Some(r) => r,
        None => loader_table
            .with_lock(capability::HandleTable::reserve_slot)
            .expect("drain freed handles_count >= 1 slots; reserve_slot cannot fail"),
    };

    Ok(commit_object_handle(
        &loader_table,
        reservation,
        CapabilityTarget::Thread(thread),
    ))
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
    let handle_id = table.with_lock(|tbl| {
        tbl.commit_reserved(reservation, Capability::new_with_default_rights(target))
    });
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
        MemoryMappingError::AlreadyMapped | MemoryMappingError::VirtualMappingError => {
            SyscallError::InvalidArgument
        }
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
    use crate::spawn_abi::{MAX_BOOTSTRAP_HANDLES, MAX_SEGMENTS_PER_IMG, USER_IMAGE_DESC_SIZE};

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
        assert_eq!(
            sys_process_start(0, 0x4000_0000, 0x4001_0000, 0, 0, 0),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn sys_process_start_rejects_too_many_handles() {
        let too_many = ((MAX_BOOTSTRAP_HANDLES as u64) + 1) << 32;
        assert_eq!(
            sys_process_start(1, 0x4000_0000, 0x4001_0000, 0, too_many, 0x2000),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn sys_process_start_rejects_reserved_bits_in_prio_word() {
        let bad = 1u64 << 8;
        assert_eq!(
            sys_process_start(1, 0x4000_0000, 0x4001_0000, 0, bad, 0),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn abi_limits_are_stable() {
        assert_eq!(MAX_SEGMENTS_PER_IMG, 16);
        assert_eq!(MAX_BOOTSTRAP_HANDLES, 32);
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
