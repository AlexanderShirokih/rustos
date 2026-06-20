//! E2E проверка per-thread IPC-буфера из EL0:
//! - main-поток получает адрес буфера через `ipc_buffer_ptr()`, пишет паттерн
//!   в `data`/`tag`/`caps` и читает обратно (round-trip);
//! - sibling-поток в том же процессе получает СВОЙ адрес через raw-syscall
//!   `ipc_buffer_addr()` - адреса двух потоков должны различаться.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use kernel_tests::kernel_test;
use runtime::{
    ipc_buffer_addr, ipc_buffer_ptr, memory_allocate, process_self, signal_wait_one, thread_create,
    thread_exit, thread_termination_signal,
};
use syscall::{IPC_BUFFER_DATA_MAX, MEM_FLAGS_READ_WRITE, SIGNALED, encode_tag};

const STACK_SIZE: u64 = 0x4000;
const JOIN_TIMEOUT_NS: u64 = 5_000_000_000;

/// Куда sibling-поток кладёт VA своего IPC-буфера (0 = ещё не записал).
static SIBLING_VA: AtomicU64 = AtomicU64::new(0);

#[kernel_test]
fn ipc_buffer_roundtrip() {
    let ptr = ipc_buffer_ptr().expect("user thread must have an ipc buffer");
    kernel_tests::kassert!(!ptr.is_null());

    // SAFETY: ядро замаппило IPC-буфер user-RW размером в одну страницу;
    // запись/чтение полей структуры лежат в его границах. Поток - один
    // владелец буфера, гонок нет.
    unsafe {
        let buf = &mut *ptr;
        buf.tag = encode_tag(IPC_BUFFER_DATA_MAX, 2);
        buf.caps[0] = 0xAABB_CCDD;
        buf.caps[1] = 0x1122_3344;
        for (i, slot) in buf.data.iter_mut().enumerate() {
            *slot = (i as u8) ^ 0x5A;
        }

        kernel_tests::kassert_eq!(buf.tag, encode_tag(IPC_BUFFER_DATA_MAX, 2));
        kernel_tests::kassert_eq!(buf.caps[0], 0xAABB_CCDD);
        kernel_tests::kassert_eq!(buf.caps[1], 0x1122_3344);
        for (i, slot) in buf.data.iter().enumerate() {
            kernel_tests::kassert_eq!(*slot, (i as u8) ^ 0x5A);
        }
    }
}

extern "C" fn sibling_worker(_arg: usize) -> ! {
    // Raw-syscall, НЕ кэширующий accessor: кэш в `runtime` процесс-глобальный,
    // а буфер per-thread - для второго потока нужен свой VA напрямую.
    let ret = ipc_buffer_addr();
    let va = u64::try_from(ret).unwrap_or(0);

    // Round-trip по собственному буферу, чтобы убедиться, что VA рабочий.
    if va != 0 {
        let ptr = va as *mut u8;
        // SAFETY: собственный per-thread буфер потока, user-RW, одна страница.
        unsafe {
            ptr.write_volatile(0xC3);
            if ptr.read_volatile() != 0xC3 {
                thread_exit(2);
            }
        }
    }

    SIBLING_VA.store(va, Relaxed);
    thread_exit(0)
}

#[kernel_test]
fn ipc_buffer_per_thread_distinct() {
    let main_ret = ipc_buffer_addr();
    kernel_tests::kassert!(main_ret > 0);
    let main_va = u64::try_from(main_ret).expect("positive va fits u64");

    let stack = memory_allocate(STACK_SIZE, MEM_FLAGS_READ_WRITE);
    kernel_tests::kassert!(stack > 0);
    let user_sp = u64::try_from(stack).expect("positive va fits u64") + STACK_SIZE;
    let process = process_self().expect("process_self handle");
    let entry = sibling_worker as extern "C" fn(usize) -> ! as *const () as u64;
    let thread = thread_create(process, entry, user_sp, 0, 1).expect("thread_create handle");

    let sig = thread_termination_signal(thread).expect("thread_termination_signal");
    let observed = signal_wait_one(sig, SIGNALED, JOIN_TIMEOUT_NS);
    kernel_tests::kassert_eq!(observed, i64::from(SIGNALED));

    let sibling_va = SIBLING_VA.load(Relaxed);
    kernel_tests::kassert!(sibling_va > 0);
    // Два потока одного процесса - разные VA per-thread буферов.
    kernel_tests::kassert!(sibling_va != main_va);
}
