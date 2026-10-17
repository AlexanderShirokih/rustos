//! E2E проверка per-thread IPC-буфера из EL0:
//! - main-поток получает адрес буфера через `ipc_buffer_ptr()`, пишет паттерн
//!   в `data`/`tag`/`caps` и читает обратно (round-trip);
//! - `ipc_buffer_ptr()` каждого потока совпадает с адресом из syscall
//!   `ipc_buffer_addr()`, а адреса двух потоков одного процесса различаются.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use runtime::{Timeout, ipc_buffer_addr, ipc_buffer_ptr, spawn};
use syscall::{IPC_BUFFER_DATA_MAX, encode_tag};

const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);

static SIBLING_VA: AtomicU64 = AtomicU64::new(0);

#[test_harness::test]
fn ipc_buffer_roundtrip() {
    let ptr = ipc_buffer_ptr().expect("user thread must have an ipc buffer");
    test_harness::check!(!ptr.is_null());

    // SAFETY: ядро замаппило IPC-буфер user-RW размером в одну страницу;
    // запись/чтение полей структуры лежат в его границах. Поток - единственный
    // владелец буфера, гонки исключены.
    unsafe {
        let buf = &mut *ptr;
        buf.tag = encode_tag(IPC_BUFFER_DATA_MAX, 2);
        buf.caps[0] = 0xAABB_CCDD;
        buf.caps[1] = 0x1122_3344;
        for (i, slot) in buf.data.iter_mut().enumerate() {
            *slot = (i as u8) ^ 0x5A;
        }

        test_harness::check_eq!(buf.tag, encode_tag(IPC_BUFFER_DATA_MAX, 2));
        test_harness::check_eq!(buf.caps[0], 0xAABB_CCDD);
        test_harness::check_eq!(buf.caps[1], 0x1122_3344);
        for (i, slot) in buf.data.iter().enumerate() {
            test_harness::check_eq!(*slot, (i as u8) ^ 0x5A);
        }
    }
}

extern "C" fn sibling_worker(_arg: usize) -> u32 {
    let ret = ipc_buffer_addr();
    let va = u64::try_from(ret).unwrap_or(0);
    if ipc_buffer_ptr().map(|ptr| ptr as u64) != Some(va) {
        return 3;
    }

    // Round-trip по собственному буферу, чтобы убедиться, что VA рабочий.
    if va != 0 {
        let ptr = va as *mut u8;
        // SAFETY: собственный per-thread буфер потока, user-RW, одна страница.
        unsafe {
            ptr.write_volatile(0xC3);
            if ptr.read_volatile() != 0xC3 {
                return 2;
            }
        }
    }

    SIBLING_VA.store(va, Relaxed);
    0
}

#[test_harness::test]
fn ipc_buffer_per_thread_distinct() {
    let main_ret = ipc_buffer_addr();
    test_harness::check!(main_ret > 0);
    let main_va = u64::try_from(main_ret).expect("positive va fits u64");
    test_harness::check_eq!(ipc_buffer_ptr().map(|ptr| ptr as u64), Some(main_va));

    let thread = spawn(sibling_worker, 0).expect("spawn sibling worker");
    thread
        .join(JOIN_TIMEOUT)
        .expect("sibling joins before timeout");

    let sibling_va = SIBLING_VA.load(Relaxed);
    test_harness::check!(sibling_va > 0);
    test_harness::check!(sibling_va != main_va);
}
