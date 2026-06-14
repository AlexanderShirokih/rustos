//! E2E проверка `ChannelCreate`/`ChannelWrite`/`ChannelRead` из EL0.

use kernel_tests::kernel_test;
use runtime::{channel_create, channel_read, channel_write, memory_allocate};

/// Round-trip байта `0x42` через свежую пару endpoint'ов; буфер выдан
/// `MemoryAllocate`. Возврат `ChannelRead` = 1: 1 байт payload, 0 handle'ов.
#[kernel_test]
fn channel_round_trip() {
    let (left, right) = channel_create().expect("channel_create must succeed");

    let va = memory_allocate(0x1000, 0);
    kernel_tests::kassert!(va > 0);
    let base = usize::try_from(va).expect("positive va fits usize");
    // SAFETY: MemoryAllocate выдал RW-страницу >= 256 байт; срез не покидает
    // тест и другим ссылкам на страницу не алиасится.
    let buf = unsafe { core::slice::from_raw_parts_mut(base as *mut u8, 256) };

    buf[0] = 0x42;
    kernel_tests::kassert_eq!(channel_write(left, &buf[..1], &[]), 0);

    buf[0] = 0;
    kernel_tests::kassert_eq!(channel_read(right, buf, &mut []), 1);
    kernel_tests::kassert_eq!(buf[0], 0x42);
}
