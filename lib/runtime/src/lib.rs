//! SVC-обёртки syscall-ABI для userspace кода.
//!
//! Аргументы уходят в x0..x5, возврат - знаковый x0: неотрицательное
//! значение - успех, отрицательное - `-(SyscallError)`.

#![no_std]
#![allow(unsafe_code)]
#![feature(alloc_error_handler)]

mod error;
mod handle;
mod heap;
mod ipc_buffer;
mod port_transport;
mod ring_transport;
mod svc;
mod sync;

pub use error::{Error, Result};
pub use handle::{BorrowedHandle, OwnedHandle};
pub use ipc_buffer::ipc_buffer_ptr;
pub use port_transport::PortTransport;
pub use ring_transport::RingTransport;
pub use svc::{
    handle_close, handle_duplicate, ipc_buffer_addr, memory_allocate, memory_create_physical,
    memory_create_virtual, memory_free, memory_map, memory_region_inspect, memory_remap, port_call,
    port_create, port_recv, port_reply, port_send, process_create, process_exit_code,
    process_load_image, process_resource_self, process_self, process_start, process_terminate,
    process_termination_signal, signal_create, signal_set, signal_wait_many, signal_wait_one,
    thread_create, thread_exit, thread_exit_code, thread_self, thread_terminate,
    thread_termination_signal,
};
pub use sync::{Condvar, Mutex, MutexGuard};

/// Глобальный heap процесса поверх memory_allocate/memory_free.
#[global_allocator]
static PROCESS_HEAP: heap::ProcessHeap = heap::ProcessHeap::new();

/// Отказ аллокации переходит в panic-handler процесса.
#[alloc_error_handler]
fn on_alloc_error(_layout: core::alloc::Layout) -> ! {
    panic!("userspace heap: allocation failed");
}
