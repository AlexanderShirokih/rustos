//! SVC-обёртки syscall-ABI для userspace кода.
//!
//! Аргументы уходят в x0..x5, возврат - знаковый x0: неотрицательное
//! значение - успех, отрицательное - `-(SyscallError)`.

#![no_std]
#![allow(unsafe_code)]

mod svc;
mod transport;

pub use svc::{
    channel_create, channel_read, channel_write, handle_close, mailbox_create, mailbox_queue,
    mailbox_wait, memory_allocate, memory_create_virtual, memory_map, memory_region_inspect,
    memory_remap, object_wait_one, process_create, process_exit_code, process_load_image,
    process_self, process_start, thread_exit, thread_self,
};
pub use transport::ChannelTransport;

/// Null-аллокатор рантайма: любая аллокация возвращает null.
struct NoHeap;

// SAFETY: alloc всегда возвращает null (отказ аллокации), поэтому dealloc
// недостижим; контракт GlobalAlloc на null-возврате соблюдён.
unsafe impl core::alloc::GlobalAlloc for NoHeap {
    unsafe fn alloc(&self, _layout: core::alloc::Layout) -> *mut u8 {
        core::ptr::null_mut()
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {}
}

#[global_allocator]
static NO_HEAP: NoHeap = NoHeap;
