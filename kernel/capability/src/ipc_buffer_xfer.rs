//! Перенос кадра и capability между IPC-буферами сторон рандеву.
//!
//! Офсеты полей [`syscall::IpcBuffer`] (`#[repr(C)]`), по которым адресуются оба
//! буфера: `tag: u64` @ 0, `caps: [u32; 4]` @ 8, `data: [u8; IPC_BUFFER_DATA_MAX]`
//! @ 24, `badge: u64` сразу за `data`.

use alloc::{sync::Arc, vec::Vec};
use core::num::NonZeroU32;

use kbase::{LockCell, MutexCell};
use memory::{memory_mapper::UserCopyError, virtual_address::VirtualAddress};
use syscall::{IPC_BUFFER_DATA_MAX, IPC_BUFFER_MAX_CAPS, Rights, decode_tag, encode_tag};

use super::{
    errors::IpcError,
    handle::{Capability, HandleId},
    handle_table::{DrainedHandles, HandleReservation, HandleTable},
    port::{BufferAccess, ThreadTransport},
};

const TAG_OFFSET: usize = 0;
const TAG_SIZE: usize = 8;
const CAPS_OFFSET: usize = TAG_OFFSET + TAG_SIZE;
const CAP_SIZE: usize = 4;
const DATA_OFFSET: usize = CAPS_OFFSET + IPC_BUFFER_MAX_CAPS * CAP_SIZE;
const BADGE_OFFSET: usize = DATA_OFFSET + IPC_BUFFER_DATA_MAX;
const BADGE_SIZE: usize = 8;

const _: () = assert!(size_of::<syscall::IpcBuffer>() <= memory::PAGE_SIZE.get());

const BODY_CHUNK: usize = 512;

/// Источник capability кадра: таблица отправителя либо набор, изъятый при постановке.
pub enum SenderCaps<'a> {
    /// Номера кадра разрешаются в таблице отправителя при доставке.
    Table,
    /// Набор принадлежит операции и расходуется только после успешной доставки.
    Staged(&'a mut Vec<Capability>),
}

pub(crate) fn copy_frame(
    sender: &ThreadTransport,
    receiver: &ThreadTransport,
) -> Result<(), IpcError> {
    let tag = syscall::decode_tag(read_tag(sender)?);
    let len = tag.len.min(IPC_BUFFER_DATA_MAX);
    let count = tag.ncaps.min(IPC_BUFFER_MAX_CAPS);
    copy_body(sender, receiver, len)?;
    for (index, raw) in read_caps(sender, count)?.into_iter().enumerate() {
        write_bytes(receiver, CAPS_OFFSET + index * CAP_SIZE, &raw.to_le_bytes())?;
    }
    let mut badge = [0; BADGE_SIZE];
    read_bytes(sender, BADGE_OFFSET, &mut badge)?;
    write_bytes(receiver, BADGE_OFFSET, &badge)?;
    write_tag(receiver, encode_tag(len, count))
}

fn map_copy_err(_e: UserCopyError) -> IpcError {
    // Непригодный user-указатель буфера трактуем как BufferTooSmall.
    IpcError::BufferTooSmall
}

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

const BUFFER_LEN: usize = BADGE_OFFSET + BADGE_SIZE;

enum BufferField {
    Tag,
    Cap(usize),
    Data(usize),
    Badge,
}

impl BufferField {
    fn classify(off: usize, len: usize) -> Result<Self, IpcError> {
        let end = off.checked_add(len).ok_or(IpcError::BufferTooSmall)?;
        if end > BUFFER_LEN {
            return Err(IpcError::BufferTooSmall);
        }
        if off >= BADGE_OFFSET {
            if off != BADGE_OFFSET || len != BADGE_SIZE {
                return Err(IpcError::BufferTooSmall);
            }
            Ok(Self::Badge)
        } else if off >= DATA_OFFSET {
            let start = off - DATA_OFFSET;
            if start + len > IPC_BUFFER_DATA_MAX {
                return Err(IpcError::BufferTooSmall);
            }
            Ok(Self::Data(start))
        } else if off >= CAPS_OFFSET {
            if !(off - CAPS_OFFSET).is_multiple_of(CAP_SIZE) || len != CAP_SIZE {
                return Err(IpcError::BufferTooSmall);
            }
            let index = (off - CAPS_OFFSET) / CAP_SIZE;
            if index >= IPC_BUFFER_MAX_CAPS {
                return Err(IpcError::BufferTooSmall);
            }
            Ok(Self::Cap(index))
        } else {
            if off != TAG_OFFSET || len != TAG_SIZE {
                return Err(IpcError::BufferTooSmall);
            }
            Ok(Self::Tag)
        }
    }
}

fn kernel_read(buf: &syscall::IpcBuffer, off: usize, dst: &mut [u8]) -> Result<(), IpcError> {
    match BufferField::classify(off, dst.len())? {
        BufferField::Tag => dst.copy_from_slice(&buf.tag.to_le_bytes()),
        BufferField::Cap(index) => {
            let cap = buf.caps.get(index).ok_or(IpcError::BufferTooSmall)?;
            dst.copy_from_slice(&cap.to_le_bytes());
        }
        BufferField::Data(start) => {
            let src = buf
                .data
                .get(start..start + dst.len())
                .ok_or(IpcError::BufferTooSmall)?;
            dst.copy_from_slice(src);
        }
        BufferField::Badge => dst.copy_from_slice(&buf.badge.to_le_bytes()),
    }
    Ok(())
}

fn kernel_write(buf: &mut syscall::IpcBuffer, off: usize, src: &[u8]) -> Result<(), IpcError> {
    let le_u64 = |src: &[u8]| {
        src.try_into()
            .map(u64::from_le_bytes)
            .map_err(|_| IpcError::BufferTooSmall)
    };
    match BufferField::classify(off, src.len())? {
        BufferField::Tag => buf.tag = le_u64(src)?,
        BufferField::Cap(index) => {
            let slot = buf.caps.get_mut(index).ok_or(IpcError::BufferTooSmall)?;
            *slot = u32::from_le_bytes(src.try_into().map_err(|_| IpcError::BufferTooSmall)?);
        }
        BufferField::Data(start) => {
            let dst = buf
                .data
                .get_mut(start..start + src.len())
                .ok_or(IpcError::BufferTooSmall)?;
            dst.copy_from_slice(src);
        }
        BufferField::Badge => buf.badge = le_u64(src)?,
    }
    Ok(())
}

pub(crate) fn read_tag(t: &ThreadTransport) -> Result<u64, IpcError> {
    let mut buf = [0u8; TAG_SIZE];
    read_bytes(t, TAG_OFFSET, &mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

pub(crate) fn write_tag(t: &ThreadTransport, tag: u64) -> Result<(), IpcError> {
    write_bytes(t, TAG_OFFSET, &tag.to_le_bytes())
}

pub(super) fn write_badge(t: &ThreadTransport, badge: u64) -> Result<(), IpcError> {
    write_bytes(t, BADGE_OFFSET, &badge.to_le_bytes())
}

pub(crate) fn read_caps(t: &ThreadTransport, ncaps: usize) -> Result<Vec<u32>, IpcError> {
    let mut out = Vec::with_capacity(ncaps);
    for i in 0..ncaps {
        let mut buf = [0u8; CAP_SIZE];
        read_bytes(t, CAPS_OFFSET + i * CAP_SIZE, &mut buf)?;
        out.push(u32::from_le_bytes(buf));
    }
    Ok(out)
}

pub(crate) fn write_caps(t: &ThreadTransport, ids: &[HandleId]) -> Result<(), IpcError> {
    for (i, id) in ids.iter().enumerate() {
        let raw = id.raw().get().to_le_bytes();
        write_bytes(t, CAPS_OFFSET + i * CAP_SIZE, &raw)?;
    }
    Ok(())
}

fn offset_va(base: VirtualAddress, off: usize) -> VirtualAddress {
    VirtualAddress::new(base.as_usize() + off)
}

fn copy_body(
    sender: &ThreadTransport,
    receiver: &ThreadTransport,
    len: usize,
) -> Result<(), IpcError> {
    // Тело копируется порциями, потому что кадр стека ядра не вмещает `IPC_BUFFER_DATA_MAX`
    // целиком.
    let mut chunk = [0u8; BODY_CHUNK];
    let mut copied = 0;
    while copied < len {
        let n = (len - copied).min(BODY_CHUNK);
        let slice = chunk.get_mut(..n).ok_or(IpcError::BufferTooSmall)?;
        read_bytes(sender, DATA_OFFSET + copied, slice)?;
        write_bytes(receiver, DATA_OFFSET + copied, slice)?;
        copied += n;
    }
    Ok(())
}

struct ReservedSlots<'t> {
    table: &'t Arc<MutexCell<HandleTable>>,
    slots: Vec<HandleReservation>,
}

impl<'t> ReservedSlots<'t> {
    fn reserve(table: &'t Arc<MutexCell<HandleTable>>, count: usize) -> Result<Self, IpcError> {
        let slots = table.with_lock(|tbl| {
            let mut slots: Vec<HandleReservation> = Vec::with_capacity(count);
            for _ in 0..count {
                match tbl.reserve_slot() {
                    Ok(slot) => slots.push(slot),
                    Err(e) => {
                        for slot in slots {
                            tbl.release_reservation(slot);
                        }
                        return Err(e);
                    }
                }
            }
            Ok(slots)
        })?;
        Ok(Self { table, slots })
    }

    fn handle_ids(&self) -> Vec<HandleId> {
        self.slots
            .iter()
            .map(HandleReservation::handle_id)
            .collect()
    }

    fn commit(mut self, handles: Vec<Capability>, rejected: &mut DrainedHandles) {
        let slots = core::mem::take(&mut self.slots);
        self.table.with_lock(|tbl| {
            for (slot, handle) in slots.into_iter().zip(handles) {
                let _ = tbl.commit_reserved(slot, handle, rejected);
            }
        });
    }
}

impl Drop for ReservedSlots<'_> {
    fn drop(&mut self) {
        if self.slots.is_empty() {
            return;
        }
        let slots = core::mem::take(&mut self.slots);
        self.table.with_lock(|tbl| {
            for slot in slots {
                tbl.release_reservation(slot);
            }
        });
    }
}

/// Переносит рандеву-сообщение из IPC-буфера `sender` в IPC-буфер `receiver`: тело (по `len` из
/// tag), значок отправителя, хендлы (по `ncaps`) и сам tag. `len` и `ncaps` урезаются до
/// `IPC_BUFFER_DATA_MAX` и `IPC_BUFFER_MAX_CAPS`.
///
/// Атомарен перенос хендлов. Ни один cap не снимается с таблицы отправителя, пока все способные
/// упасть записи в буфер получателя не прошли успешно, и каждый из них требует
/// [`Rights::TRANSFER`]. Тело пишется до тега, и на отказе получатель остаётся с прежним тегом,
/// а недописанное тело кадром для него не становится.
pub fn transfer_rendezvous(
    sender: &ThreadTransport,
    receiver: &ThreadTransport,
    caps: SenderCaps<'_>,
) -> Result<(), IpcError> {
    let tag = read_tag(sender)?;
    let decoded = decode_tag(tag);
    let len = decoded.len.min(IPC_BUFFER_DATA_MAX);
    let ncaps = decoded.ncaps.min(IPC_BUFFER_MAX_CAPS);

    copy_body(sender, receiver, len)?;

    write_badge(receiver, sender.badge)?;

    if ncaps > 0 {
        transfer_caps(sender, receiver, ncaps, caps)?;
    }

    // Тег пишется последним, поэтому до него частичное тело не наблюдается получателем как
    // принятое сообщение.
    write_tag(receiver, encode_tag(len, ncaps))?;
    Ok(())
}

/// Закрывает capability доставленного кадра в таблице получателя и обнуляет тег его буфера.
///
/// Приём, отказавший после переноса, кадр получателю не отдаёт, и набор capability этого кадра
/// остаётся без владельца. Тег обнуляется до закрытия, и доставленный кадр получатель не
/// наблюдает ни на одном исходе. Неизвестные и уже закрытые id набора пропускаются.
pub fn release_delivered(receiver: &ThreadTransport) -> Result<(), IpcError> {
    let tag = read_tag(receiver)?;
    let ncaps = decode_tag(tag).ncaps.min(IPC_BUFFER_MAX_CAPS);
    let ids = read_caps(receiver, ncaps)?;
    // Сброс тега предшествует закрытию, и получатель не наблюдает доставленный кадр ни на
    // одном исходе.
    write_tag(receiver, encode_tag(0, 0))?;

    for raw in ids {
        let Some(nz) = NonZeroU32::new(raw) else {
            continue;
        };
        let drained = receiver
            .handle_table
            .with_lock(|tbl| tbl.remove(HandleId::from_raw(nz)));
        if let Ok(drained) = drained {
            drained.release();
        }
    }
    Ok(())
}

fn transfer_caps(
    sender: &ThreadTransport,
    receiver: &ThreadTransport,
    ncaps: usize,
    caps: SenderCaps<'_>,
) -> Result<(), IpcError> {
    let mut ids = Vec::new();
    match &caps {
        SenderCaps::Staged(staged) => {
            if staged.len() != ncaps {
                return Err(IpcError::BadHandle);
            }
        }
        SenderCaps::Table => {
            for raw in read_caps(sender, ncaps)? {
                let nz = NonZeroU32::new(raw).ok_or(IpcError::BadHandle)?;
                ids.push(HandleId::from_raw(nz));
            }
        }
    }

    let reserved = ReservedSlots::reserve(&receiver.handle_table, ncaps)?;
    write_caps(receiver, &reserved.handle_ids())?;

    let mut drained = DrainedHandles::empty();
    // Изъятие у отправителя идёт после всех способных упасть шагов на стороне получателя.
    let taken = match caps {
        SenderCaps::Table => sender
            .handle_table
            .with_lock(|table| table.try_drain_for_transfer(&ids, Rights::TRANSFER, &mut drained))
            .map(|()| drained.take_handles()),
        SenderCaps::Staged(staged) => Ok(core::mem::take(staged)),
    };
    match taken {
        Ok(handles) => {
            reserved.commit(handles, &mut drained);
            drained.release();
            Ok(())
        }
        Err(e) => {
            drained.release();
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use kbase::{LockCell, MutexCell};
    use memory::{
        MemFlags, PAGE_SIZE, memory_mapper::MemoryMapper, testing::PageMapper,
        virtual_address::VirtualAddress,
    };
    use syscall::encode_tag;

    use super::{
        super::{
            handle::Capability, handle_table::HandleTable, port::ThreadTransport, signal::Signal,
            target::CapabilityTarget,
        },
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
        let mapper = PageMapper::with_mapping(base, 1, &[], MemFlags::user_rw()).unwrap();
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

        transfer_rendezvous(&sender, &receiver, SenderCaps::Table).expect("transfer ok");

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
                tbl.insert(Capability::new(
                    CapabilityTarget::Signal(notif.clone()),
                    Rights::READ | Rights::TRANSFER,
                ))
            })
            .unwrap();
        write_buf(&sm, BASE_A, encode_tag(2, 1), &[id.raw().get()], b"hi");

        transfer_rendezvous(&sender, &receiver, SenderCaps::Table).expect("transfer ok");

        assert_eq!(st.with_lock(|t| t.live_count()), 0);
        assert_eq!(rt.with_lock(|t| t.live_count()), 1);
        assert_eq!(read_tag(&rm, BASE_B), encode_tag(2, 1));

        let mut idb = [0u8; 4];
        rm.copy_user_in(VirtualAddress::new(BASE_B + CAPS_OFFSET), &mut idb)
            .unwrap();
        let new_raw = u32::from_le_bytes(idb);
        let new_id = handle_id_from_raw(new_raw);
        rt.with_lock(|tbl| {
            let got = tbl.get(new_id, Rights::READ).unwrap();
            let CapabilityTarget::Signal(got_signal) = got.target() else {
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

        transfer_rendezvous(&sender, &receiver, SenderCaps::Table).expect("transfer ok");

        assert_eq!(read_badge(&rm, BASE_B), 0xDEAD_BEEF);
    }

    #[test]
    fn zero_badge_delivered_when_sender_unbadged() {
        let (sender, sm, _st) = transport(BASE_A);
        let (receiver, rm, _rt) = transport(BASE_B);
        write_buf(&sm, BASE_A, encode_tag(3, 0), &[], b"abc");

        transfer_rendezvous(&sender, &receiver, SenderCaps::Table).expect("transfer ok");

        assert_eq!(read_badge(&rm, BASE_B), 0);
    }

    #[test]
    fn cap_without_transfer_right_fails_atomically() {
        let (sender, sm, st) = transport(BASE_A);
        let (receiver, _rm, rt) = transport(BASE_B);

        // Signal только с READ - перенос должен отказать.
        let id = st
            .with_lock(|tbl| {
                tbl.insert(Capability::new(
                    CapabilityTarget::Signal(Signal::new()),
                    Rights::READ,
                ))
            })
            .unwrap();
        write_buf(&sm, BASE_A, encode_tag(0, 1), &[id.raw().get()], b"");

        let err = transfer_rendezvous(&sender, &receiver, SenderCaps::Table).unwrap_err();
        assert_eq!(err, IpcError::AccessDenied);
        assert_eq!(st.with_lock(|t| t.live_count()), 1);
        assert_eq!(rt.with_lock(|t| t.live_count()), 0);
    }

    fn handle_id_from_raw(raw: u32) -> HandleId {
        HandleId::from_raw(core::num::NonZeroU32::new(raw).unwrap())
    }

    fn transport_custom(
        size: usize,
        cap: u32,
    ) -> (
        ThreadTransport,
        Arc<PageMapper>,
        Arc<MutexCell<HandleTable>>,
    ) {
        let mapper =
            PageMapper::with_mapping(BASE_B - PAGE_SIZE.get(), 1, &[], MemFlags::user_rw())
                .unwrap();
        let table = Arc::new(MutexCell::new(HandleTable::with_capacity(cap)));
        let t = ThreadTransport::new(
            mapper.clone(),
            VirtualAddress::new(BASE_B - size),
            table.clone(),
        );
        (t, mapper, table)
    }

    fn signal_with_transfer(table: &Arc<MutexCell<HandleTable>>) -> HandleId {
        table
            .with_lock(|tbl| {
                tbl.insert(Capability::new(
                    CapabilityTarget::Signal(Signal::new()),
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

        transfer_rendezvous(&sender, &receiver, SenderCaps::Table).expect("transfer ok");

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

        transfer_rendezvous(&sender, &receiver, SenderCaps::Table).expect("transfer ok");

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

        transfer_rendezvous(&sender, &receiver, SenderCaps::Table).expect("transfer ok");

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

        transfer_rendezvous(&sender, &receiver, SenderCaps::Table).expect("transfer ok");

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

        transfer_rendezvous(&sender, &receiver, SenderCaps::Table).expect("transfer ok");

        assert_eq!(read_tag(&rm, BASE_B), encode_tag(3, 0));
        assert_eq!(read_data(&rm, BASE_B, 3), b"abc");
    }

    #[test]
    fn install_fail_rolls_caps_back_to_sender() {
        let (sender, sm, st) = transport(BASE_A);
        let (receiver, _rm, rt) = transport_custom(4096, 1);
        let id_a = signal_with_transfer(&st);
        let id_b = signal_with_transfer(&st);
        write_buf(
            &sm,
            BASE_A,
            encode_tag(0, 2),
            &[id_a.raw().get(), id_b.raw().get()],
            b"",
        );

        let err = transfer_rendezvous(&sender, &receiver, SenderCaps::Table).unwrap_err();
        assert_eq!(err, IpcError::OutOfHandles);
        assert!(st.with_lock(|t| t.get(id_a, Rights::READ).is_ok()));
        assert!(st.with_lock(|t| t.get(id_b, Rights::READ).is_ok()));
        assert_eq!(st.with_lock(|t| t.live_count()), 2);
        assert_eq!(rt.with_lock(|t| t.live_count()), 0);
    }

    #[test]
    fn write_caps_fail_rolls_caps_back_to_sender() {
        let (sender, sm, st) = transport(BASE_A);
        let (receiver, _rm, rt) = transport_custom(CAPS_OFFSET, 16);
        let id = signal_with_transfer(&st);
        write_buf(&sm, BASE_A, encode_tag(0, 1), &[id.raw().get()], b"");

        let err = transfer_rendezvous(&sender, &receiver, SenderCaps::Table).unwrap_err();
        assert_eq!(err, IpcError::BufferTooSmall);
        assert!(st.with_lock(|t| t.get(id, Rights::READ).is_ok()));
        assert_eq!(st.with_lock(|t| t.live_count()), 1);
        assert_eq!(rt.with_lock(|t| t.live_count()), 0);
    }

    #[test]
    fn write_badge_fail_leaves_receiver_tag_unset() {
        let (mut sender, sm, _st) = transport(BASE_A);
        sender.badge = 0x1234_5678;
        let (receiver, rm, _rt) = transport_custom(BADGE_OFFSET, 16);
        write_buf(&sm, BASE_A, encode_tag(4, 0), &[], b"DATA");

        let err = transfer_rendezvous(&sender, &receiver, SenderCaps::Table).unwrap_err();
        assert_eq!(err, IpcError::BufferTooSmall);
        assert_eq!(read_tag(&rm, BASE_B - BADGE_OFFSET), 0);
        assert_eq!(read_data(&rm, BASE_B - BADGE_OFFSET, 4), b"DATA");
    }

    #[test]
    fn write_badge_fail_rolls_transferred_caps_back_to_sender() {
        let (mut sender, sm, st) = transport(BASE_A);
        sender.badge = 0xABCD;
        let (receiver, rm, rt) = transport_custom(BADGE_OFFSET, 16);
        let id = signal_with_transfer(&st);
        write_buf(&sm, BASE_A, encode_tag(0, 1), &[id.raw().get()], b"");

        let err = transfer_rendezvous(&sender, &receiver, SenderCaps::Table).unwrap_err();
        assert_eq!(err, IpcError::BufferTooSmall);
        assert!(st.with_lock(|t| t.get(id, Rights::READ).is_ok()));
        assert_eq!(st.with_lock(|t| t.live_count()), 1);
        assert_eq!(rt.with_lock(|t| t.live_count()), 0);
        assert_eq!(read_tag(&rm, BASE_B - BADGE_OFFSET), 0);
    }

    type KernelBuf = Arc<MutexCell<syscall::IpcBuffer>>;
}
