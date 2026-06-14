//! SVC-обёртки syscall-ABI для userspace кода.
//!
//! Аргументы уходят в x0..x5, возврат - знаковый x0: неотрицательное
//! значение - успех, отрицательное - `-(SyscallError)`.

#![no_std]
#![allow(unsafe_code)]
#![feature(alloc_error_handler)]

mod heap;
mod svc;
mod sync;
mod transport;

pub use svc::{
    channel_create, channel_read, channel_write, event_create, handle_close, handle_duplicate,
    mailbox_cancel, mailbox_create, mailbox_queue, mailbox_wait, mailbox_wait_async,
    memory_allocate, memory_create_physical, memory_create_virtual, memory_free, memory_map,
    memory_region_inspect, memory_remap, object_signal, object_wait_many, object_wait_one,
    process_create, process_exit_code, process_load_image, process_self, process_start,
    process_terminate, thread_create, thread_exit, thread_exit_code, thread_self, thread_terminate,
};
pub use sync::{Condvar, Mutex, MutexGuard};
pub use transport::ChannelTransport;

/// Глобальный heap процесса поверх memory_allocate/memory_free.
#[global_allocator]
static PROCESS_HEAP: heap::ProcessHeap = heap::ProcessHeap::new();

/// Отказ аллокации переходит в panic-handler процесса.
#[alloc_error_handler]
fn on_alloc_error(_layout: core::alloc::Layout) -> ! {
    panic!("userspace heap: allocation failed");
}
