//! E2E проверка `ChannelCreate`/`ChannelWrite`/`ChannelRead` из EL0.

use kernel_tests::kernel_test;
use runtime::{channel_create, channel_read, channel_write, memory_allocate};

/// Round-trip байта `0x42` через свежую пару endpoint'ов; буфер выдан
/// `MemoryAllocate`. Возврат `ChannelRead` = 1: 1 байт payload, 0 handle'ов.
#[kernel_test]
fn channel_round_trip() {
    let (left_ret, right) = channel_create();
    kernel_tests::kassert!(left_ret > 0);
    kernel_tests::kassert!(right > 0);
    let left = usize::try_from(left_ret).expect("positive handle fits usize");
    let right = usize::try_from(right).expect("handle fits usize");

    let va = memory_allocate(0x1000, 0);
    kernel_tests::kassert!(va > 0);
    let base = usize::try_from(va).expect("positive va fits usize");
    // SAFETY: MemoryAllocate выдал RW-страницу >= 256 байт; срез не покидает
    // тест и другим ссылкам на страницу не алиасится.
    let buf = unsafe { core::slice::from_raw_parts_mut(base as *mut u8, 256) };

    buf[0] = 0x42;
    kernel_tests::kassert_eq!(channel_write(left, &buf[..1]), 0);

    buf[0] = 0;
    kernel_tests::kassert_eq!(channel_read(right, buf), 1);
    kernel_tests::kassert_eq!(buf[0], 0x42);
}
