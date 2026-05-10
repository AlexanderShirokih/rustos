//! Handler-ы mailbox-syscall'ов. Register-flat ABI: 32-байтный
//! [`MailboxPacket`](kobject::MailboxPacket) копируется через
//! user-указатель в обе стороны; сериализация - руками по offset'ам, без
//! `unsafe`-кастов на `repr(C)`-структуру.

use kobject::{
    self, AsyncMode, MAILBOX_PACKET_SIZE, MAILBOX_PAYLOAD_SIZE, MailboxPacket, MailboxPacketKind,
};

use super::{
    bridge::parse_handle_id,
    error::SyscallError,
    runtime::runtime as syscall_runtime,
    user_io::{copy_in, copy_out, validate_user_ptr},
};

/// Wire-offset'ы полей пакета. Часть syscall ABI: должны совпадать с
/// layout'ом `repr(C)`-[`MailboxPacket`].
const OFFSET_KEY: usize = 0;
const OFFSET_KIND: usize = 8;
const OFFSET_STATUS: usize = 12;
const OFFSET_PAYLOAD: usize = 16;

pub(super) fn sys_mailbox_create() -> Result<u64, SyscallError> {
    let id = kobject::mailbox_create()?;
    Ok(u64::from(id.raw().get()))
}

pub(super) fn sys_mailbox_queue(
    handle: u64,
    packet_va: u64,
    packet_len: u64,
) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let len = usize::try_from(packet_len).map_err(|_| SyscallError::MessageTooBig)?;
    if len < MAILBOX_PACKET_SIZE {
        return Err(SyscallError::BufferTooSmall);
    }
    if len > MAILBOX_PACKET_SIZE {
        return Err(SyscallError::MessageTooBig);
    }
    validate_user_ptr(packet_va, MAILBOX_PACKET_SIZE)?;

    let user_vm = syscall_runtime()
        .current_user_vm()
        .ok_or(SyscallError::WrongType)?;

    let mut buf = [0u8; MAILBOX_PACKET_SIZE];
    copy_in(&user_vm, packet_va, &mut buf)?;

    let packet = decode_user_packet(&buf)?;
    kobject::mailbox_queue(id, packet)?;
    Ok(0)
}

pub(super) fn sys_mailbox_wait(
    handle: u64,
    timeout_ns: u64,
    packet_va: u64,
    packet_cap: u64,
) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let cap = usize::try_from(packet_cap).map_err(|_| SyscallError::BufferTooSmall)?;
    if cap < MAILBOX_PACKET_SIZE {
        return Err(SyscallError::BufferTooSmall);
    }
    validate_user_ptr(packet_va, MAILBOX_PACKET_SIZE)?;

    let user_vm = syscall_runtime()
        .current_user_vm()
        .ok_or(SyscallError::WrongType)?;

    let packet = kobject::mailbox_wait(id, Some(timeout_ns))?;

    let buf = encode_packet(&packet);
    copy_out(&user_vm, packet_va, &buf)?;
    Ok(MAILBOX_PACKET_SIZE as u64)
}

pub(super) fn sys_mailbox_wait_async(
    mbox: u64,
    target: u64,
    key: u64,
    mask_and_mode: u64,
) -> Result<u64, SyscallError> {
    let mbox_id = parse_handle_id(mbox)?;
    let target_id = parse_handle_id(target)?;

    let mask = u32::try_from(mask_and_mode & u64::from(u32::MAX))
        .expect("masking guarantees value fits into u32");
    if mask == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    let mode_raw = u32::try_from((mask_and_mode >> 32) & u64::from(u32::MAX))
        .expect("masking guarantees value fits into u32");
    let mode = match mode_raw {
        0 => AsyncMode::Once,
        1 => AsyncMode::Repeating,
        _ => return Err(SyscallError::InvalidArgument),
    };

    kobject::mailbox_wait_async(mbox_id, target_id, key, mask, mode)?;
    Ok(0)
}

pub(super) fn sys_mailbox_cancel(mbox: u64, target: u64, key: u64) -> Result<u64, SyscallError> {
    let mbox_id = parse_handle_id(mbox)?;
    let target_id = parse_handle_id(target)?;
    kobject::mailbox_cancel(mbox_id, target_id, key)?;
    Ok(0)
}

/// Десериализация пакета из user-памяти. User-у разрешён только
/// `User`-пакет: signal-пакеты ставит ядро через observer-механику, и
/// прокидывание их через syscall дало бы возможность подделать
/// `(target_koid, key)`-пары в очереди.
fn decode_user_packet(buf: &[u8; MAILBOX_PACKET_SIZE]) -> Result<MailboxPacket, SyscallError> {
    let key = u64::from_le_bytes(
        buf[OFFSET_KEY..OFFSET_KEY + 8]
            .try_into()
            .expect("8 bytes at fixed offset"),
    );
    // User-у разрешён только User-пакет: signal-пакеты (kind 1/2)
    // ставит ядро через observer-механику, и допустить их подделку
    // через syscall - значит дать перепутать `(target_koid, key)`-пары.
    let kind = match buf[OFFSET_KIND] {
        0 => MailboxPacketKind::User,
        _ => return Err(SyscallError::InvalidArgument),
    };
    let status = i32::from_le_bytes(
        buf[OFFSET_STATUS..OFFSET_STATUS + 4]
            .try_into()
            .expect("4 bytes at fixed offset"),
    );
    let mut payload = [0u8; MAILBOX_PAYLOAD_SIZE];
    payload.copy_from_slice(&buf[OFFSET_PAYLOAD..OFFSET_PAYLOAD + MAILBOX_PAYLOAD_SIZE]);
    Ok(MailboxPacket {
        key,
        kind,
        status,
        payload,
    })
}

fn encode_packet(packet: &MailboxPacket) -> [u8; MAILBOX_PACKET_SIZE] {
    let mut buf = [0u8; MAILBOX_PACKET_SIZE];
    buf[OFFSET_KEY..OFFSET_KEY + 8].copy_from_slice(&packet.key.to_le_bytes());
    buf[OFFSET_KIND] = match packet.kind {
        MailboxPacketKind::User => 0,
        MailboxPacketKind::SignalOnce => 1,
        MailboxPacketKind::SignalRepeating => 2,
    };
    buf[OFFSET_STATUS..OFFSET_STATUS + 4].copy_from_slice(&packet.status.to_le_bytes());
    buf[OFFSET_PAYLOAD..OFFSET_PAYLOAD + MAILBOX_PAYLOAD_SIZE].copy_from_slice(&packet.payload);
    buf
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    fn user_packet_buf(kind: u8) -> [u8; MAILBOX_PACKET_SIZE] {
        let mut buf = [0u8; MAILBOX_PACKET_SIZE];
        buf[OFFSET_KEY..OFFSET_KEY + 8].copy_from_slice(&0xCAFE_BABE_u64.to_le_bytes());
        buf[OFFSET_KIND] = kind;
        buf
    }

    #[test]
    fn mailbox_queue_with_zero_len_returns_buffer_too_small() {
        let r = sys_mailbox_queue(1, 0x1000, 0);
        assert_eq!(r, Err(SyscallError::BufferTooSmall));
    }

    #[test]
    fn mailbox_queue_with_oversize_packet_rejected() {
        let r = sys_mailbox_queue(1, 0x1000, MAILBOX_PACKET_SIZE as u64 + 1);
        assert_eq!(r, Err(SyscallError::MessageTooBig));
    }

    #[test]
    fn mailbox_queue_with_huge_len_rejected() {
        let r = sys_mailbox_queue(1, 0x1000, u64::MAX);
        assert_eq!(r, Err(SyscallError::MessageTooBig));
    }

    #[test]
    fn mailbox_queue_with_zero_handle_invalid() {
        let r = sys_mailbox_queue(0, 0x1000, MAILBOX_PACKET_SIZE as u64);
        assert_eq!(r, Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn mailbox_wait_with_undersize_buffer_rejected() {
        let r = sys_mailbox_wait(1, 0, 0x1000, MAILBOX_PACKET_SIZE as u64 - 1);
        assert_eq!(r, Err(SyscallError::BufferTooSmall));
    }

    #[test]
    fn mailbox_wait_with_zero_handle_invalid() {
        let r = sys_mailbox_wait(0, 0, 0x1000, MAILBOX_PACKET_SIZE as u64);
        assert_eq!(r, Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn mailbox_wait_async_with_zero_mask_rejected() {
        let r = sys_mailbox_wait_async(1, 2, 0, 0);
        assert_eq!(r, Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn mailbox_wait_async_with_unknown_mode_rejected() {
        // mask = 1, mode = 99 -> upper 32 bits.
        let mask_and_mode: u64 = 1 | (99u64 << 32);
        let r = sys_mailbox_wait_async(1, 2, 0, mask_and_mode);
        assert_eq!(r, Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn mailbox_wait_async_with_zero_handle_invalid() {
        let r = sys_mailbox_wait_async(0, 2, 0, 1);
        assert_eq!(r, Err(SyscallError::InvalidArgument));
        let r = sys_mailbox_wait_async(1, 0, 0, 1);
        assert_eq!(r, Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn mailbox_cancel_with_zero_handle_invalid() {
        let r = sys_mailbox_cancel(0, 2, 0);
        assert_eq!(r, Err(SyscallError::InvalidArgument));
        let r = sys_mailbox_cancel(1, 0, 0);
        assert_eq!(r, Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn decode_user_packet_accepts_user_kind() {
        let buf = user_packet_buf(0);
        let pkt = decode_user_packet(&buf).expect("user kind decodes");
        assert_eq!(pkt.kind, MailboxPacketKind::User);
        assert_eq!(pkt.key, 0xCAFE_BABE);
        assert_eq!(pkt.status, 0);
    }

    #[test]
    fn mailbox_queue_with_signal_once_kind_rejected() {
        let buf = user_packet_buf(1);
        assert_eq!(
            decode_user_packet(&buf).err(),
            Some(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn mailbox_queue_with_signal_repeating_kind_rejected() {
        let buf = user_packet_buf(2);
        assert_eq!(
            decode_user_packet(&buf).err(),
            Some(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn mailbox_queue_with_unknown_kind_rejected() {
        let buf = user_packet_buf(99);
        assert_eq!(
            decode_user_packet(&buf).err(),
            Some(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn encode_decode_round_trip_user() {
        let payload = {
            let mut p = [0u8; MAILBOX_PAYLOAD_SIZE];
            for (i, slot) in p.iter_mut().enumerate() {
                *slot = i as u8;
            }
            p
        };
        let pkt = MailboxPacket {
            key: 0x0123_4567_89AB_CDEF,
            kind: MailboxPacketKind::User,
            status: -42,
            payload,
        };
        let buf = encode_packet(&pkt);
        let back = decode_user_packet(&buf).expect("user kind round-trips");
        assert_eq!(back.key, pkt.key);
        assert_eq!(back.kind, pkt.kind);
        assert_eq!(back.status, pkt.status);
        assert_eq!(back.payload, pkt.payload);
    }

    #[test]
    fn encode_signal_packet_writes_kind_byte() {
        let pkt = MailboxPacket {
            key: 1,
            kind: MailboxPacketKind::SignalOnce,
            status: 0,
            payload: [0; MAILBOX_PAYLOAD_SIZE],
        };
        let buf = encode_packet(&pkt);
        assert_eq!(buf[OFFSET_KIND], 1);

        let pkt = MailboxPacket {
            key: 1,
            kind: MailboxPacketKind::SignalRepeating,
            status: 0,
            payload: [0; MAILBOX_PAYLOAD_SIZE],
        };
        let buf = encode_packet(&pkt);
        assert_eq!(buf[OFFSET_KIND], 2);
    }
}
