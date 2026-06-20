//! Кросс-AS перенос rendezvous-сообщения между IPC-buffer'ами двух потоков.
//!
//! Используется [`Port`](super::port) и [`Reply`](super::reply).
//! Тело копируется через линейное физ-отображение (`copy_user_in`/
//! `copy_user_out`) без переключения TTBR0 - mapper любого AS читает/пишет
//! user-страницы по своему дереву трансляции. Хендлы переносятся атомарно
//! (drain из таблицы отправителя с `Rights::TRANSFER`, install в таблицу
//! получателя с откатом - паттерн `try_drain_for_transfer` + install_or_restore
//! из channel-пути, но кросс-процессно).

use alloc::vec::Vec;
use core::num::NonZeroU32;

use collections::LockCell;
use memory::{memory_mapper::UserCopyError, virtual_address::VirtualAddress};
use syscall::{IPC_BUFFER_DATA_MAX, IPC_BUFFER_MAX_CAPS, decode_tag, encode_tag};

use super::{
    errors::IpcError,
    handle::{Handle, HandleId},
    handle_table::HandleTable,
    port::{BufferAccess, ThreadTransport},
    rights::Rights,
};

/// ABI-офсеты полей [`syscall::IpcBuffer`] (`#[repr(C)]`):
/// `tag: u64` @ 0,
/// `caps: [u32; 4]` @ 8,
/// `data: [u8; 256]` @ 24,
/// `badge: u64` @ 280 (сразу за `data`).
const TAG_OFFSET: usize = 0;
const TAG_SIZE: usize = 8;
const CAPS_OFFSET: usize = TAG_OFFSET + TAG_SIZE;
const CAP_SIZE: usize = 4;
const DATA_OFFSET: usize = CAPS_OFFSET + IPC_BUFFER_MAX_CAPS * CAP_SIZE;
const BADGE_OFFSET: usize = DATA_OFFSET + IPC_BUFFER_DATA_MAX;
const BADGE_SIZE: usize = 8;

fn map_copy_err(_e: UserCopyError) -> IpcError {
    // Непригодный user-указатель буфера трактуем как BufferTooSmall.
    IpcError::BufferTooSmall
}

/// Читает `dst.len()` байт тела буфера со смещения `off` (ABI-офсет внутри
/// [`syscall::IpcBuffer`]). Диспетчеризует по виду доступа: User - через
/// mapper, Kernel - прямым доступом к kernel-резидентному буферу.
fn read_bytes(t: &ThreadTransport, off: usize, dst: &mut [u8]) -> Result<(), IpcError> {
    match &t.access {
        BufferAccess::User {
            mapper,
            ipc_buffer_va,
        } => mapper
            .copy_user_in(offset_va(*ipc_buffer_va, off), dst)
            .map_err(map_copy_err),
        BufferAccess::Kernel { buffer } => buffer.with_lock(|buf| kernel_read(buf, off, dst)),
    }
}

/// Пишет `src` в тело буфера со смещения `off`. Диспетчер по виду доступа,
/// см. [`read_bytes`].
fn write_bytes(t: &ThreadTransport, off: usize, src: &[u8]) -> Result<(), IpcError> {
    match &t.access {
        BufferAccess::User {
            mapper,
            ipc_buffer_va,
        } => mapper
            .copy_user_out(offset_va(*ipc_buffer_va, off), src)
            .map_err(map_copy_err),
        BufferAccess::Kernel { buffer } => buffer.with_lock(|buf| kernel_write(buf, off, src)),
    }
}

/// Линейный размер kernel-резидентного буфера в байтах.
const BUFFER_LEN: usize = BADGE_OFFSET + BADGE_SIZE;

fn kernel_read(buf: &syscall::IpcBuffer, off: usize, dst: &mut [u8]) -> Result<(), IpcError> {
    let end = off.checked_add(dst.len()).ok_or(IpcError::BufferTooSmall)?;
    if end > BUFFER_LEN {
        return Err(IpcError::BufferTooSmall);
    }
    if off >= BADGE_OFFSET {
        if off != BADGE_OFFSET || dst.len() != BADGE_SIZE {
            return Err(IpcError::BufferTooSmall);
        }
        dst.copy_from_slice(&buf.badge.to_le_bytes());
    } else if off >= DATA_OFFSET {
        let start = off - DATA_OFFSET;
        let src = buf
            .data
            .get(start..start + dst.len())
            .ok_or(IpcError::BufferTooSmall)?;
        dst.copy_from_slice(src);
    } else if off >= CAPS_OFFSET {
        // Чтение из области caps: только выровненное по слоту 4-байтовое поле.
        let (i, src) = caps_slot(buf, off, dst.len())?;
        let _ = i;
        dst.copy_from_slice(&src);
    } else {
        if off != TAG_OFFSET || dst.len() != TAG_SIZE {
            return Err(IpcError::BufferTooSmall);
        }
        dst.copy_from_slice(&buf.tag.to_le_bytes());
    }
    Ok(())
}

fn kernel_write(buf: &mut syscall::IpcBuffer, off: usize, src: &[u8]) -> Result<(), IpcError> {
    let end = off.checked_add(src.len()).ok_or(IpcError::BufferTooSmall)?;
    if end > BUFFER_LEN {
        return Err(IpcError::BufferTooSmall);
    }
    if off >= BADGE_OFFSET {
        if off != BADGE_OFFSET || src.len() != BADGE_SIZE {
            return Err(IpcError::BufferTooSmall);
        }
        buf.badge = u64::from_le_bytes(src.try_into().map_err(|_| IpcError::BufferTooSmall)?);
    } else if off >= DATA_OFFSET {
        let start = off - DATA_OFFSET;
        let dst = buf
            .data
            .get_mut(start..start + src.len())
            .ok_or(IpcError::BufferTooSmall)?;
        dst.copy_from_slice(src);
    } else if off >= CAPS_OFFSET {
        if !(off - CAPS_OFFSET).is_multiple_of(CAP_SIZE) || src.len() != CAP_SIZE {
            return Err(IpcError::BufferTooSmall);
        }
        let i = (off - CAPS_OFFSET) / CAP_SIZE;
        let slot = buf.caps.get_mut(i).ok_or(IpcError::BufferTooSmall)?;
        *slot = u32::from_le_bytes(src.try_into().map_err(|_| IpcError::BufferTooSmall)?);
    } else {
        if off != TAG_OFFSET || src.len() != TAG_SIZE {
            return Err(IpcError::BufferTooSmall);
        }
        buf.tag = u64::from_le_bytes(src.try_into().map_err(|_| IpcError::BufferTooSmall)?);
    }
    Ok(())
}

/// Возвращает индекс caps-слота и его 4 байта LE для чтения; проверяет
/// выравнивание и размер.
fn caps_slot(
    buf: &syscall::IpcBuffer,
    off: usize,
    len: usize,
) -> Result<(usize, [u8; CAP_SIZE]), IpcError> {
    if !(off - CAPS_OFFSET).is_multiple_of(CAP_SIZE) || len != CAP_SIZE {
        return Err(IpcError::BufferTooSmall);
    }
    let i = (off - CAPS_OFFSET) / CAP_SIZE;
    let v = buf.caps.get(i).ok_or(IpcError::BufferTooSmall)?;
    Ok((i, v.to_le_bytes()))
}

fn read_tag(t: &ThreadTransport) -> Result<u64, IpcError> {
    let mut buf = [0u8; TAG_SIZE];
    read_bytes(t, TAG_OFFSET, &mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn write_tag(t: &ThreadTransport, tag: u64) -> Result<(), IpcError> {
    write_bytes(t, TAG_OFFSET, &tag.to_le_bytes())
}

fn write_badge(t: &ThreadTransport, badge: u64) -> Result<(), IpcError> {
    write_bytes(t, BADGE_OFFSET, &badge.to_le_bytes())
}

fn read_caps(t: &ThreadTransport, ncaps: usize) -> Result<Vec<u32>, IpcError> {
    let mut out = Vec::with_capacity(ncaps);
    for i in 0..ncaps {
        let mut buf = [0u8; CAP_SIZE];
        read_bytes(t, CAPS_OFFSET + i * CAP_SIZE, &mut buf)?;
        out.push(u32::from_le_bytes(buf));
    }
    Ok(out)
}

fn write_caps(t: &ThreadTransport, ids: &[HandleId]) -> Result<(), IpcError> {
    for (i, id) in ids.iter().enumerate() {
        let raw = id.raw().get().to_le_bytes();
        write_bytes(t, CAPS_OFFSET + i * CAP_SIZE, &raw)?;
    }
    Ok(())
}

fn offset_va(base: VirtualAddress, off: usize) -> VirtualAddress {
    VirtualAddress::new(base.as_usize() + off)
}

/// Переносит рандеву-сообщение из IPC-буфера `sender` в IPC-буфер
/// `receiver`: тело (по `len` из tag), хендлы (по `ncaps`), и tag.
///
/// Атомарность caps: drain из таблицы отправителя (требуя
/// [`Rights::TRANSFER`]) и install в таблицу получателя выполняются с
/// откатом - на любой ошибке install уже вставленные хендлы возвращаются
/// отправителю, а у получателя ничего не остаётся. На ошибке тело может
/// быть уже скопировано в data-область получателя, но tag НЕ выставлен,
/// поэтому получатель увидит исходный (нулевой) tag и не примет мусор.
pub fn transfer_rendezvous(
    sender: &ThreadTransport,
    receiver: &ThreadTransport,
) -> Result<(), IpcError> {
    let tag = read_tag(sender)?;
    let (len, ncaps) = decode_tag(tag);
    let len = len.min(IPC_BUFFER_DATA_MAX);
    let ncaps = ncaps.min(IPC_BUFFER_MAX_CAPS);

    if len > 0 {
        let mut body = [0u8; IPC_BUFFER_DATA_MAX];
        read_bytes(sender, DATA_OFFSET, &mut body[..len])?;
        write_bytes(receiver, DATA_OFFSET, &body[..len])?;
    }

    // 2. Caps: читаем HandleId'ы отправителя, drain из его таблицы (требуя
    //    TRANSFER), install в таблицу получателя с откатом.
    if ncaps > 0 {
        let raw_ids = read_caps(sender, ncaps)?;
        let mut ids: Vec<HandleId> = Vec::with_capacity(ncaps);
        for raw in raw_ids {
            let nz = NonZeroU32::new(raw).ok_or(IpcError::BadHandle)?;
            ids.push(HandleId::from_raw(nz));
        }

        // Атомарный drain из source-таблицы (всё или ничего).
        let drained = sender
            .handle_table
            .with_lock(|tbl| tbl.try_drain_for_transfer(&ids, Rights::TRANSFER))?;

        // Install в receiver-таблицу с откатом; на ошибке вернуть всё в
        // source-таблицу.
        let new_ids: Vec<HandleId> = match receiver
            .handle_table
            .with_lock(|tbl| install_all(tbl, drained))
        {
            Ok(ids) => ids,
            Err((e, returned)) => {
                // Возвращаем хендлы обратно отправителю, чтобы перенос был
                // полностью откатан (caller получит ошибку, его хендлы целы).
                sender.handle_table.with_lock(|tbl| {
                    for h in returned {
                        // Слоты заведомо свободны (мы их только что
                        // дренировали), insert не должен упасть; если упал -
                        // хендл закрывается (Arc -> 0), что безопасно.
                        let _ = tbl.insert(h);
                    }
                });
                return Err(e);
            }
        };

        // Пишем новые HandleId'ы в caps получателя.
        if let Err(e) = write_caps(receiver, &new_ids) {
            // Копирование caps в user-буфер получателя провалилось - откат:
            // вынимаем из receiver-таблицы и возвращаем отправителю.
            let restored: Vec<Handle> = receiver.handle_table.with_lock(|tbl| {
                let mut v = Vec::with_capacity(new_ids.len());
                for id in &new_ids {
                    if let Ok(h) = tbl.remove(*id) {
                        v.push(h);
                    }
                }
                v
            });
            sender.handle_table.with_lock(|tbl| {
                for h in restored {
                    let _ = tbl.insert(h);
                }
            });
            return Err(e);
        }
    }

    // 3. Финально - tag получателю (len + ncaps). Только после успешного
    //    тела и caps.
    write_tag(receiver, encode_tag(len, ncaps))?;

    // 4. Значок отправителя - получателю. Пишется независимо от тела/caps
    //    (даже при len==0/ncaps==0): сервер различает клиентов по badge.
    write_badge(receiver, sender.badge)?;
    Ok(())
}

/// Тестовый in-memory mapper: одна страница user-памяти под IPC-buffer.
/// `copy_user_in/out` режут по `[base_va, base_va + len)`.
#[cfg(test)]
pub(crate) mod test_mapper {
    use alloc::{sync::Arc, vec, vec::Vec};

    use collections::MutexCell;
    use memory::{
        MemFlags,
        memory_mapper::{
            AddressSpaceHandle, AddressSpaceTag, MemoryMapper, MemoryMappingError,
            MemoryRemappingError, MemoryUnmappingError, UserCopyError,
        },
        physical_address::{PageAlignedAddress, PhysicalAddress},
        virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
    };

    pub(crate) struct PageMapper {
        base: usize,
        bytes: MutexCell<Vec<u8>>,
    }

    impl PageMapper {
        pub(crate) fn new(base: usize) -> Arc<Self> {
            Arc::new(Self {
                base,
                bytes: MutexCell::new(vec![0u8; 4096]),
            })
        }
    }

    impl MemoryMapper for PageMapper {
        fn map(
            &self,
            _va: PageAlignedVirtualAddress,
            _pc: usize,
            _init: &[u8],
            _f: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            unimplemented!()
        }
        fn map_exact(
            &self,
            _v: PageAlignedVirtualAddress,
            _p: PageAlignedAddress,
            _s: usize,
            _f: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            unimplemented!()
        }
        fn unmap(
            &self,
            _v: PageAlignedVirtualAddress,
            _s: usize,
        ) -> Result<(), MemoryUnmappingError> {
            unimplemented!()
        }
        fn remap(
            &self,
            _v: PageAlignedVirtualAddress,
            _s: usize,
            _f: MemFlags,
        ) -> Result<(), MemoryRemappingError> {
            unimplemented!()
        }
        fn activate_handle(&self) -> AddressSpaceHandle {
            AddressSpaceHandle::new(PhysicalAddress::new(0), AddressSpaceTag::NONE)
        }
        fn zero_owned_frame(&self, _p: PageAlignedAddress) {}

        fn copy_user_in(&self, va: VirtualAddress, dst: &mut [u8]) -> Result<(), UserCopyError> {
            use collections::LockCell;
            let off = va.as_usize().wrapping_sub(self.base);
            let end = off.checked_add(dst.len()).ok_or(UserCopyError::NotMapped)?;
            self.bytes.with_lock(|b| {
                if end > b.len() {
                    return Err(UserCopyError::NotMapped);
                }
                dst.copy_from_slice(&b[off..end]);
                Ok(())
            })
        }

        fn copy_user_out(&self, va: VirtualAddress, src: &[u8]) -> Result<(), UserCopyError> {
            use collections::LockCell;
            let off = va.as_usize().wrapping_sub(self.base);
            let end = off.checked_add(src.len()).ok_or(UserCopyError::NotMapped)?;
            self.bytes.with_lock(|b| {
                if end > b.len() {
                    return Err(UserCopyError::NotMapped);
                }
                b[off..end].copy_from_slice(src);
                Ok(())
            })
        }

        fn as_any(&self) -> &(dyn core::any::Any + 'static) {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use collections::{LockCell, MutexCell};
    use memory::{memory_mapper::MemoryMapper, virtual_address::VirtualAddress};
    use syscall::encode_tag;

    use super::{
        super::{
            handle::Handle, handle_table::HandleTable, object::KObject, port::ThreadTransport,
            rights::Rights, signal::Signal,
        },
        test_mapper::PageMapper,
        *,
    };

    const BASE_A: usize = 0x4000_0000;
    const BASE_B: usize = 0x5000_0000;

    fn transport(
        base: usize,
    ) -> (
        ThreadTransport,
        Arc<PageMapper>,
        Arc<MutexCell<HandleTable>>,
    ) {
        let mapper = PageMapper::new(base);
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let t = ThreadTransport::new(mapper.clone(), VirtualAddress::new(base), table.clone());
        (t, mapper, table)
    }

    fn write_buf(mapper: &Arc<PageMapper>, base: usize, tag: u64, caps: &[u32], data: &[u8]) {
        // tag @0, caps @8, data @24.
        mapper
            .copy_user_out(VirtualAddress::new(base), &tag.to_le_bytes())
            .unwrap();
        for (i, c) in caps.iter().enumerate() {
            mapper
                .copy_user_out(VirtualAddress::new(base + 8 + i * 4), &c.to_le_bytes())
                .unwrap();
        }
        mapper
            .copy_user_out(VirtualAddress::new(base + 24), data)
            .unwrap();
    }

    fn read_tag(mapper: &Arc<PageMapper>, base: usize) -> u64 {
        let mut b = [0u8; 8];
        mapper
            .copy_user_in(VirtualAddress::new(base), &mut b)
            .unwrap();
        u64::from_le_bytes(b)
    }

    fn read_data(mapper: &Arc<PageMapper>, base: usize, len: usize) -> alloc::vec::Vec<u8> {
        let mut v = alloc::vec![0u8; len];
        mapper
            .copy_user_in(VirtualAddress::new(base + 24), &mut v)
            .unwrap();
        v
    }

    #[test]
    fn body_round_trip_no_caps() {
        let (sender, sm, _st) = transport(BASE_A);
        let (receiver, rm, _rt) = transport(BASE_B);
        write_buf(&sm, BASE_A, encode_tag(5, 0), &[], b"hello");

        transfer_rendezvous(&sender, &receiver).expect("transfer ok");

        assert_eq!(read_tag(&rm, BASE_B), encode_tag(5, 0));
        assert_eq!(read_data(&rm, BASE_B, 5), b"hello");
    }

    #[test]
    fn cap_transfer_moves_handle_and_writes_new_id() {
        let (sender, sm, st) = transport(BASE_A);
        let (receiver, rm, rt) = transport(BASE_B);

        let notif = Signal::new();
        let koid = KObject::Signal(notif.clone()).koid();
        let id = st
            .with_lock(|tbl| {
                tbl.insert(Handle::new(
                    KObject::Signal(notif),
                    Rights::READ | Rights::TRANSFER,
                ))
            })
            .unwrap();
        write_buf(&sm, BASE_A, encode_tag(2, 1), &[id.raw().get()], b"hi");

        transfer_rendezvous(&sender, &receiver).expect("transfer ok");

        assert_eq!(st.with_lock(|t| t.live_count()), 0);
        assert_eq!(rt.with_lock(|t| t.live_count()), 1);
        assert_eq!(read_tag(&rm, BASE_B), encode_tag(2, 1));

        // Новый HandleId записан в caps[0] получателя.
        let mut idb = [0u8; 4];
        rm.copy_user_in(VirtualAddress::new(BASE_B + 8), &mut idb)
            .unwrap();
        let new_raw = u32::from_le_bytes(idb);
        let new_id = kobject_handle_id(new_raw);
        let got = rt
            .with_lock(|tbl| tbl.get(new_id, Rights::READ).map(|h| h.koid()))
            .unwrap();
        assert_eq!(got, koid);
    }

    fn read_badge(mapper: &Arc<PageMapper>, base: usize) -> u64 {
        // badge @ 280 (tag@0 + caps 16 + data 256).
        let mut b = [0u8; 8];
        mapper
            .copy_user_in(VirtualAddress::new(base + 280), &mut b)
            .unwrap();
        u64::from_le_bytes(b)
    }

    #[test]
    fn badge_delivered_to_receiver() {
        let (mut sender, sm, _st) = transport(BASE_A);
        let (receiver, rm, _rt) = transport(BASE_B);

        sender.badge = 0xDEAD_BEEF;
        write_buf(&sm, BASE_A, encode_tag(0, 0), &[], b"");

        transfer_rendezvous(&sender, &receiver).expect("transfer ok");

        assert_eq!(read_badge(&rm, BASE_B), 0xDEAD_BEEF);
    }

    #[test]
    fn zero_badge_delivered_when_sender_unbadged() {
        let (sender, sm, _st) = transport(BASE_A);
        let (receiver, rm, _rt) = transport(BASE_B);
        write_buf(&sm, BASE_A, encode_tag(3, 0), &[], b"abc");

        transfer_rendezvous(&sender, &receiver).expect("transfer ok");

        assert_eq!(read_badge(&rm, BASE_B), 0);
    }

    #[test]
    fn cap_without_transfer_right_fails_atomically() {
        let (sender, sm, st) = transport(BASE_A);
        let (receiver, _rm, rt) = transport(BASE_B);

        // Signal только с READ - перенос должен отказать.
        let id = st
            .with_lock(|tbl| tbl.insert(Handle::new(KObject::Signal(Signal::new()), Rights::READ)))
            .unwrap();
        write_buf(&sm, BASE_A, encode_tag(0, 1), &[id.raw().get()], b"");

        let err = transfer_rendezvous(&sender, &receiver).unwrap_err();
        assert_eq!(err, IpcError::AccessDenied);
        assert_eq!(st.with_lock(|t| t.live_count()), 1);
        assert_eq!(rt.with_lock(|t| t.live_count()), 0);
    }

    fn kobject_handle_id(raw: u32) -> HandleId {
        HandleId::from_raw(core::num::NonZeroU32::new(raw).unwrap())
    }
}

/// Вставляет все `handles` в `tbl`. На ошибке install'а возвращает уже
/// вставленные обратно как `Handle`-ы вместе с тем, что не вставился и
/// остатком - вызывающий вернёт их источнику.
fn install_all(
    tbl: &mut HandleTable,
    handles: Vec<Handle>,
) -> Result<Vec<HandleId>, (IpcError, Vec<Handle>)> {
    let mut inserted: Vec<HandleId> = Vec::with_capacity(handles.len());
    let mut iter = handles.into_iter();
    while let Some(handle) = iter.next() {
        match tbl.try_insert(handle) {
            Ok(id) => inserted.push(id),
            Err((e, returned)) => {
                // Откат: вынимаем уже вставленные обратно как Handle.
                let mut back: Vec<Handle> = Vec::with_capacity(inserted.len() + 1);
                for id in &inserted {
                    if let Ok(h) = tbl.remove(*id) {
                        back.push(h);
                    }
                }
                back.push(returned);
                for h in iter {
                    back.push(h);
                }
                return Err((e, back));
            }
        }
    }
    Ok(inserted)
}
