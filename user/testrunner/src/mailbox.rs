//! E2E проверка `MailboxCreate`/`MailboxQueue`/`MailboxWait` из EL0.

use kernel_tests::kernel_test;
use syscall::MAILBOX_PACKET_SIZE;
use runtime::{mailbox_create, mailbox_queue, mailbox_wait, memory_allocate};

const KEY: u64 = 0xCAFE_BABE;

/// Round-trip user-пакета с известным `key` через свежий mailbox; пакет
/// лежит в странице от `MemoryAllocate` (свежевыделенная - обнулена, поэтому
/// kind=User=0, status=0 и payload проходят валидацию).
#[kernel_test]
fn mailbox_round_trip() {
    let mbox = mailbox_create().expect("mailbox handle");

    let va = memory_allocate(0x1000, 0);
    kernel_tests::kassert!(va > 0);
    let base = usize::try_from(va).expect("positive va fits usize");
    // SAFETY: MemoryAllocate выдал RW-страницу >= MAILBOX_PACKET_SIZE байт;
    // срез не покидает тест и другим ссылкам на страницу не алиасится.
    let packet = unsafe { core::slice::from_raw_parts_mut(base as *mut u8, MAILBOX_PACKET_SIZE) };

    packet[..8].copy_from_slice(&KEY.to_le_bytes());
    kernel_tests::kassert_eq!(mailbox_queue(mbox, packet), 0);

    packet.fill(0);
    let packet_len = i64::try_from(MAILBOX_PACKET_SIZE).expect("packet size fits i64");
    kernel_tests::kassert_eq!(mailbox_wait(mbox, 0, packet), packet_len);
    let readback = u64::from_le_bytes(packet[..8].try_into().expect("8-byte key prefix"));
    kernel_tests::kassert_eq!(readback, KEY);
}
