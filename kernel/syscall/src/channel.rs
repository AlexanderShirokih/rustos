//! Handler-ы channel-syscall'ов. Register-flat ABI: payload-байты и
//! массив `HandleId` приходят по user-указателю; копирование между AS
//! идёт через [`MemoryMapper::copy_user_in`]/`copy_user_out`.
//!
//! [`MemoryMapper::copy_user_in`]: memory::memory_mapper::MemoryMapper::copy_user_in

use core::num::NonZeroU32;

use collections::LockCell;
use kobject::{
    HandleId, IpcError, MESSAGE_INLINE_MAX, MESSAGE_MAX_HANDLES, Message, Rights, runtime,
};

use super::{
    bridge::parse_handle_id,
    error::SyscallError,
    runtime::runtime as syscall_runtime,
    user_io::{copy_in, copy_out, validate_user_ptr},
};

/// Размер записи `HandleId` в user-памяти - именно `u32`, ABI-стабильно.
const HANDLE_ID_SIZE: usize = core::mem::size_of::<u32>();

pub(super) fn sys_channel_write(
    handle: u64,
    bytes_va: u64,
    bytes_len: u64,
    handles_va: u64,
    handles_count: u64,
) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let bytes_len = parse_len(bytes_len, MESSAGE_INLINE_MAX, SyscallError::MessageTooBig)?;
    let handles_count = parse_len(
        handles_count,
        MESSAGE_MAX_HANDLES,
        SyscallError::MessageTooBig,
    )?;
    validate_user_ptr(bytes_va, bytes_len)?;
    validate_user_ptr(handles_va, handles_count)?;

    let user_vm = syscall_runtime()
        .current_user_vm()
        .ok_or(SyscallError::WrongType)?;

    let mut bytes_buf = [0u8; MESSAGE_INLINE_MAX];
    if bytes_len > 0 {
        copy_in(&user_vm, bytes_va, &mut bytes_buf[..bytes_len])?;
    }

    let mut id_buf = [0u8; MESSAGE_MAX_HANDLES * HANDLE_ID_SIZE];
    if handles_count > 0 {
        copy_in(
            &user_vm,
            handles_va,
            &mut id_buf[..handles_count * HANDLE_ID_SIZE],
        )?;
    }
    let mut ids =
        [HandleId::from_raw(NonZeroU32::new(1).expect("non-zero literal")); MESSAGE_MAX_HANDLES];
    for (slot, raw_chunk) in ids
        .iter_mut()
        .zip(id_buf.chunks_exact(HANDLE_ID_SIZE))
        .take(handles_count)
    {
        let raw = u32::from_le_bytes(raw_chunk.try_into().expect("4 bytes per id"));
        let nz = NonZeroU32::new(raw).ok_or(SyscallError::InvalidArgument)?;
        *slot = HandleId::from_raw(nz);
    }
    let ids = &ids[..handles_count];

    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let endpoint = table
        .with_lock(|tbl| tbl.get_channel(id, Rights::WRITE))
        .map_err(SyscallError::from)?;

    let outer = endpoint.try_write::<IpcError>(|| {
        let mut msg = Message::from_bytes(&bytes_buf[..bytes_len])?;
        let drained = table.with_lock(|tbl| tbl.try_drain_for_transfer(ids, Rights::TRANSFER))?;
        for h in drained {
            msg.push_handle(h)?;
        }
        Ok(msg)
    });

    match outer {
        Ok(Ok(())) => Ok(0),
        Ok(Err(e)) | Err(e) => Err(SyscallError::from(e)),
    }
}

pub(super) fn sys_channel_read(
    handle: u64,
    bytes_va: u64,
    bytes_cap: u64,
    handles_va: u64,
    handles_cap: u64,
) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let bytes_cap = parse_len(bytes_cap, MESSAGE_INLINE_MAX, SyscallError::BufferTooSmall)?;
    let handles_cap = parse_len(
        handles_cap,
        MESSAGE_MAX_HANDLES,
        SyscallError::BufferTooSmall,
    )?;
    validate_user_ptr(bytes_va, bytes_cap)?;
    validate_user_ptr(handles_va, handles_cap)?;

    let user_vm = syscall_runtime()
        .current_user_vm()
        .ok_or(SyscallError::WrongType)?;

    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let endpoint = table
        .with_lock(|tbl| tbl.get_channel(id, Rights::READ))
        .map_err(SyscallError::from)?;

    // try_read коммитит pop ровно один раз, и только если finalize вернул
    // Ok. Любая ошибка внутри (caps, copy_out, install) оставляет
    // сообщение в очереди в исходном состоянии (handle'ы возвращены
    // через push_handle, table-вставки откачены).
    let mut bytes_len = 0usize;
    let mut handles_count = 0usize;
    let outcome = endpoint.try_read::<SyscallError>(|msg| {
        if msg.bytes().len() > bytes_cap || msg.handles_count() > handles_cap {
            return Err(SyscallError::BufferTooSmall);
        }
        bytes_len = msg.bytes().len();
        handles_count = msg.handles_count();

        if bytes_len > 0 {
            copy_out(&user_vm, bytes_va, msg.bytes())?;
        }

        if handles_count == 0 {
            return Ok(());
        }

        let new_ids = table
            .with_lock(|tbl| install_or_restore(msg, tbl))
            .map_err(SyscallError::from)?;

        let mut id_buf = [0u8; MESSAGE_MAX_HANDLES * HANDLE_ID_SIZE];
        for (i, hid) in new_ids.iter().enumerate() {
            id_buf[i * HANDLE_ID_SIZE..(i + 1) * HANDLE_ID_SIZE]
                .copy_from_slice(&hid.raw().get().to_le_bytes());
        }
        if let Err(e) = copy_out(
            &user_vm,
            handles_va,
            &id_buf[..handles_count * HANDLE_ID_SIZE],
        ) {
            // Откатываем install: вынимаем из table и кладём обратно в msg.
            // msg остаётся в очереди в исходном состоянии после ранней
            // отдачи Err из finalize.
            table.with_lock(|tbl| {
                for hid in &new_ids {
                    if let Ok(handle) = tbl.remove(*hid) {
                        let _ = msg.push_handle(handle);
                    }
                }
            });
            return Err(e);
        }
        Ok(())
    });

    match outcome {
        Ok(Ok(_msg)) => Ok(
            u64::try_from(bytes_len).expect("bounded by MESSAGE_INLINE_MAX")
                | (u64::try_from(handles_count).expect("bounded by MESSAGE_MAX_HANDLES") << 32),
        ),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(SyscallError::from(e)),
    }
}

/// Перемещает все handle'ы из `msg` в `tbl`. На любой ошибке install'а -
/// возвращает уже вставленные обратно в `msg` (через `remove` +
/// `push_handle`), а handle, на котором сломались - тоже (через
/// `try_insert`'овский return-on-error). msg возвращается к исходному
/// набору handle'ов.
fn install_or_restore(
    msg: &mut Message,
    tbl: &mut kobject::HandleTable,
) -> Result<alloc::vec::Vec<HandleId>, IpcError> {
    let drained: alloc::vec::Vec<_> = msg.drain_handles().collect();
    let mut inserted: alloc::vec::Vec<HandleId> = alloc::vec::Vec::with_capacity(drained.len());
    let mut iter = drained.into_iter();
    while let Some(handle) = iter.next() {
        match tbl.try_insert(handle) {
            Ok(hid) => inserted.push(hid),
            Err((e, returned)) => {
                for hid in &inserted {
                    if let Ok(restored) = tbl.remove(*hid) {
                        msg.push_handle(restored)
                            .expect("capacity restored by drain");
                    }
                }
                msg.push_handle(returned)
                    .expect("capacity restored by drain");
                for h in iter {
                    msg.push_handle(h).expect("capacity restored by drain");
                }
                return Err(e);
            }
        }
    }
    Ok(inserted)
}

fn parse_len(raw: u64, max: usize, overflow_err: SyscallError) -> Result<usize, SyscallError> {
    let n = usize::try_from(raw).map_err(|_| overflow_err)?;
    if n > max {
        return Err(overflow_err);
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn parse_len_zero_passes() {
        assert_eq!(parse_len(0, 256, SyscallError::MessageTooBig), Ok(0));
    }

    #[test]
    fn parse_len_at_limit_passes() {
        assert_eq!(parse_len(256, 256, SyscallError::MessageTooBig), Ok(256));
    }

    #[test]
    fn parse_len_over_limit_returns_overflow_err() {
        assert_eq!(
            parse_len(257, 256, SyscallError::MessageTooBig),
            Err(SyscallError::MessageTooBig)
        );
        assert_eq!(
            parse_len(257, 256, SyscallError::BufferTooSmall),
            Err(SyscallError::BufferTooSmall)
        );
    }

    #[test]
    fn parse_len_huge_returns_overflow_err() {
        assert_eq!(
            parse_len(u64::MAX, 256, SyscallError::MessageTooBig),
            Err(SyscallError::MessageTooBig)
        );
    }
}
