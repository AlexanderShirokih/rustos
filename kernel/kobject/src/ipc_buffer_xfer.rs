//! Кросс-AS перенос рандеву-сообщения между IPC-буферами двух потоков.

use alloc::vec::Vec;
use core::num::NonZeroU32;

use collections::LockCell;
use memory::{memory_mapper::UserCopyError, virtual_address::VirtualAddress};
use syscall::{IPC_BUFFER_DATA_MAX, IPC_BUFFER_MAX_CAPS, decode_tag, encode_tag};

use super::{
    errors::IpcError,
    handle::HandleId,
    handle_table::{HandleReservation, HandleTable},
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
        let src = caps_slot(buf, off, dst.len())?;
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

fn caps_slot(buf: &syscall::IpcBuffer, off: usize, len: usize) -> Result<[u8; CAP_SIZE], IpcError> {
    if !(off - CAPS_OFFSET).is_multiple_of(CAP_SIZE) || len != CAP_SIZE {
        return Err(IpcError::BufferTooSmall);
    }
    let i = (off - CAPS_OFFSET) / CAP_SIZE;
    let v = buf.caps.get(i).ok_or(IpcError::BufferTooSmall)?;
    Ok(v.to_le_bytes())
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
/// Атомарность передачи: ни один cap не снимается с таблицы отправителя, пока
/// все способные упасть записи в буфер получателя не прошли успешно.
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

    write_badge(receiver, sender.badge)?;

    if ncaps > 0 {
        let raw_ids = read_caps(sender, ncaps)?;
        let mut ids: Vec<HandleId> = Vec::with_capacity(ncaps);
        for raw in raw_ids {
            let nz = NonZeroU32::new(raw).ok_or(IpcError::BadHandle)?;
            ids.push(HandleId::from_raw(nz));
        }

        // Резервируем ncaps слотов получателя (всё или ничего).
        let reservations = receiver
            .handle_table
            .with_lock(|tbl| reserve_slots(tbl, ncaps))?;
        let predicted: Vec<HandleId> = reservations.iter().map(|r| r.handle_id()).collect();

        // Пишем предсказанные ID в caps получателя. На ошибке освобождаем
        // резервации - отправитель ещё не тронут.
        if let Err(e) = write_caps(receiver, &predicted) {
            receiver
                .handle_table
                .with_lock(|tbl| release_all(tbl, &reservations));
            return Err(e);
        }

        let drained = match sender
            .handle_table
            .with_lock(|tbl| tbl.try_drain_for_transfer(&ids, Rights::TRANSFER))
        {
            Ok(d) => d,
            Err(e) => {
                receiver
                    .handle_table
                    .with_lock(|tbl| release_all(tbl, &reservations));
                return Err(e);
            }
        };

        receiver.handle_table.with_lock(|tbl| {
            for (res, handle) in reservations.into_iter().zip(drained) {
                let _ = tbl.commit_reserved(res, handle);
            }
        });
    }

    write_tag(receiver, encode_tag(len, ncaps))?;
    Ok(())
}

fn reserve_slots(tbl: &mut HandleTable, n: usize) -> Result<Vec<HandleReservation>, IpcError> {
    let mut reservations: Vec<HandleReservation> = Vec::with_capacity(n);
    for _ in 0..n {
        match tbl.reserve_slot() {
            Ok(r) => reservations.push(r),
            Err(e) => {
                release_all(tbl, &reservations);
                return Err(e);
            }
        }
    }
    Ok(reservations)
}

fn release_all(tbl: &mut HandleTable, reservations: &[HandleReservation]) {
    for r in reservations {
        tbl.release_reservation(*r);
    }
}

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
            Self::with_size(base, 4096)
        }

        pub(crate) fn with_size(base: usize, size: usize) -> Arc<Self> {
            Arc::new(Self {
                base,
                bytes: MutexCell::new(vec![0u8; size]),
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
        mapper
            .copy_user_out(VirtualAddress::new(base + TAG_OFFSET), &tag.to_le_bytes())
            .unwrap();
        for (i, c) in caps.iter().enumerate() {
            mapper
                .copy_user_out(
                    VirtualAddress::new(base + CAPS_OFFSET + i * CAP_SIZE),
                    &c.to_le_bytes(),
                )
                .unwrap();
        }
        mapper
            .copy_user_out(VirtualAddress::new(base + DATA_OFFSET), data)
            .unwrap();
    }

    fn read_tag(mapper: &Arc<PageMapper>, base: usize) -> u64 {
        let mut b = [0u8; 8];
        mapper
            .copy_user_in(VirtualAddress::new(base + TAG_OFFSET), &mut b)
            .unwrap();
        u64::from_le_bytes(b)
    }

    fn read_data(mapper: &Arc<PageMapper>, base: usize, len: usize) -> alloc::vec::Vec<u8> {
        let mut v = alloc::vec![0u8; len];
        mapper
            .copy_user_in(VirtualAddress::new(base + DATA_OFFSET), &mut v)
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
        let id = st
            .with_lock(|tbl| {
                tbl.insert(Handle::new(
                    KObject::Signal(notif.clone()),
                    Rights::READ | Rights::TRANSFER,
                ))
            })
            .unwrap();
        write_buf(&sm, BASE_A, encode_tag(2, 1), &[id.raw().get()], b"hi");

        transfer_rendezvous(&sender, &receiver).expect("transfer ok");

        assert_eq!(st.with_lock(|t| t.live_count()), 0);
        assert_eq!(rt.with_lock(|t| t.live_count()), 1);
        assert_eq!(read_tag(&rm, BASE_B), encode_tag(2, 1));

        let mut idb = [0u8; 4];
        rm.copy_user_in(VirtualAddress::new(BASE_B + CAPS_OFFSET), &mut idb)
            .unwrap();
        let new_raw = u32::from_le_bytes(idb);
        let new_id = kobject_handle_id(new_raw);
        rt.with_lock(|tbl| {
            let got = tbl.get(new_id, Rights::READ).unwrap();
            let KObject::Signal(got_signal) = got.object() else {
                panic!("transferred handle must keep object type");
            };
            assert!(Arc::ptr_eq(got_signal, &notif));
        });
    }

    fn read_badge(mapper: &Arc<PageMapper>, base: usize) -> u64 {
        let mut b = [0u8; 8];
        mapper
            .copy_user_in(VirtualAddress::new(base + BADGE_OFFSET), &mut b)
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

    fn transport_custom(
        base: usize,
        size: usize,
        cap: u32,
    ) -> (
        ThreadTransport,
        Arc<PageMapper>,
        Arc<MutexCell<HandleTable>>,
    ) {
        let mapper = PageMapper::with_size(base, size);
        let table = Arc::new(MutexCell::new(HandleTable::with_capacity(cap)));
        let t = ThreadTransport::new(mapper.clone(), VirtualAddress::new(base), table.clone());
        (t, mapper, table)
    }

    fn signal_with_transfer(table: &Arc<MutexCell<HandleTable>>) -> HandleId {
        table
            .with_lock(|tbl| {
                tbl.insert(Handle::new(
                    KObject::Signal(Signal::new()),
                    Rights::READ | Rights::TRANSFER,
                ))
            })
            .unwrap()
    }

    #[test]
    fn body_exactly_at_data_max_round_trips() {
        let (sender, sm, _st) = transport(BASE_A);
        let (receiver, rm, _rt) = transport(BASE_B);
        let body = alloc::vec![0x5Au8; IPC_BUFFER_DATA_MAX];
        write_buf(&sm, BASE_A, encode_tag(IPC_BUFFER_DATA_MAX, 0), &[], &body);

        transfer_rendezvous(&sender, &receiver).expect("transfer ok");

        assert_eq!(read_tag(&rm, BASE_B), encode_tag(IPC_BUFFER_DATA_MAX, 0));
        assert_eq!(read_data(&rm, BASE_B, IPC_BUFFER_DATA_MAX), body);
    }

    #[test]
    fn oversized_len_is_clamped_to_data_max() {
        let (sender, sm, _st) = transport(BASE_A);
        let (receiver, rm, _rt) = transport(BASE_B);
        let body = alloc::vec![0xABu8; IPC_BUFFER_DATA_MAX];
        write_buf(
            &sm,
            BASE_A,
            encode_tag(IPC_BUFFER_DATA_MAX + 100, 0),
            &[],
            &body,
        );

        transfer_rendezvous(&sender, &receiver).expect("transfer ok");

        assert_eq!(read_tag(&rm, BASE_B), encode_tag(IPC_BUFFER_DATA_MAX, 0));
        assert_eq!(read_data(&rm, BASE_B, IPC_BUFFER_DATA_MAX), body);
    }

    #[test]
    fn oversized_ncaps_is_clamped_to_max_caps() {
        let (sender, sm, st) = transport(BASE_A);
        let (receiver, rm, rt) = transport(BASE_B);
        let mut raw_ids = alloc::vec::Vec::new();
        for _ in 0..IPC_BUFFER_MAX_CAPS {
            raw_ids.push(signal_with_transfer(&st).raw().get());
        }
        write_buf(
            &sm,
            BASE_A,
            encode_tag(0, IPC_BUFFER_MAX_CAPS + 5),
            &raw_ids,
            b"",
        );

        transfer_rendezvous(&sender, &receiver).expect("transfer ok");

        assert_eq!(read_tag(&rm, BASE_B), encode_tag(0, IPC_BUFFER_MAX_CAPS));
        assert_eq!(st.with_lock(|t| t.live_count()), 0);
        assert_eq!(rt.with_lock(|t| t.live_count()), IPC_BUFFER_MAX_CAPS);
    }

    #[test]
    fn kernel_buffer_round_trips_body_and_badge() {
        let (mut sender, sm, _st) = transport(BASE_A);
        sender.badge = 0xCAFE_F00D;
        let kbuf: KernelBuf = Arc::new(MutexCell::new(syscall::IpcBuffer::zeroed()));
        let rtable = Arc::new(MutexCell::new(HandleTable::new()));
        let receiver = ThreadTransport::new_kernel(kbuf.clone(), rtable);
        write_buf(&sm, BASE_A, encode_tag(4, 0), &[], b"PING");

        transfer_rendezvous(&sender, &receiver).expect("transfer ok");

        let (tag, data, badge) = kbuf.with_lock(|b| (b.tag, b.data, b.badge));
        assert_eq!(tag, encode_tag(4, 0));
        assert_eq!(&data[..4], b"PING");
        assert_eq!(badge, 0xCAFE_F00D);
    }

    #[test]
    fn kernel_sender_body_read_through_kernel_access() {
        let kbuf: KernelBuf = Arc::new(MutexCell::new(syscall::IpcBuffer::zeroed()));
        kbuf.with_lock(|b| {
            b.tag = encode_tag(3, 0);
            b.data[..3].copy_from_slice(b"abc");
        });
        let stable = Arc::new(MutexCell::new(HandleTable::new()));
        let sender = ThreadTransport::new_kernel(kbuf, stable);
        let (receiver, rm, _rt) = transport(BASE_B);

        transfer_rendezvous(&sender, &receiver).expect("transfer ok");

        assert_eq!(read_tag(&rm, BASE_B), encode_tag(3, 0));
        assert_eq!(read_data(&rm, BASE_B, 3), b"abc");
    }

    #[test]
    fn install_fail_rolls_caps_back_to_sender() {
        let (sender, sm, st) = transport(BASE_A);
        let (receiver, _rm, rt) = transport_custom(BASE_B, 4096, 1);
        let id_a = signal_with_transfer(&st);
        let id_b = signal_with_transfer(&st);
        write_buf(
            &sm,
            BASE_A,
            encode_tag(0, 2),
            &[id_a.raw().get(), id_b.raw().get()],
            b"",
        );

        let err = transfer_rendezvous(&sender, &receiver).unwrap_err();
        assert_eq!(err, IpcError::OutOfHandles);
        assert!(st.with_lock(|t| t.get(id_a, Rights::READ).is_ok()));
        assert!(st.with_lock(|t| t.get(id_b, Rights::READ).is_ok()));
        assert_eq!(st.with_lock(|t| t.live_count()), 2);
        assert_eq!(rt.with_lock(|t| t.live_count()), 0);
    }

    #[test]
    fn write_caps_fail_rolls_caps_back_to_sender() {
        let (sender, sm, st) = transport(BASE_A);
        let (receiver, _rm, rt) = transport_custom(BASE_B, CAPS_OFFSET, 16);
        let id = signal_with_transfer(&st);
        write_buf(&sm, BASE_A, encode_tag(0, 1), &[id.raw().get()], b"");

        let err = transfer_rendezvous(&sender, &receiver).unwrap_err();
        assert_eq!(err, IpcError::BufferTooSmall);
        assert!(st.with_lock(|t| t.get(id, Rights::READ).is_ok()));
        assert_eq!(st.with_lock(|t| t.live_count()), 1);
        assert_eq!(rt.with_lock(|t| t.live_count()), 0);
    }

    #[test]
    fn write_badge_fail_leaves_receiver_tag_unset() {
        let (mut sender, sm, _st) = transport(BASE_A);
        sender.badge = 0x1234_5678;
        let (receiver, rm, _rt) = transport_custom(BASE_B, BADGE_OFFSET, 16);
        write_buf(&sm, BASE_A, encode_tag(4, 0), &[], b"DATA");

        let err = transfer_rendezvous(&sender, &receiver).unwrap_err();
        assert_eq!(err, IpcError::BufferTooSmall);
        assert_eq!(read_tag(&rm, BASE_B), 0);
        assert_eq!(read_data(&rm, BASE_B, 4), b"DATA");
    }

    #[test]
    fn write_badge_fail_rolls_transferred_caps_back_to_sender() {
        let (mut sender, sm, st) = transport(BASE_A);
        sender.badge = 0xABCD;
        let (receiver, rm, rt) = transport_custom(BASE_B, BADGE_OFFSET, 16);
        let id = signal_with_transfer(&st);
        write_buf(&sm, BASE_A, encode_tag(0, 1), &[id.raw().get()], b"");

        let err = transfer_rendezvous(&sender, &receiver).unwrap_err();
        assert_eq!(err, IpcError::BufferTooSmall);
        assert!(st.with_lock(|t| t.get(id, Rights::READ).is_ok()));
        assert_eq!(st.with_lock(|t| t.live_count()), 1);
        assert_eq!(rt.with_lock(|t| t.live_count()), 0);
        assert_eq!(read_tag(&rm, BASE_B), 0);
    }

    type KernelBuf = Arc<MutexCell<syscall::IpcBuffer>>;
}
