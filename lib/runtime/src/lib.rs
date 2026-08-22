//! Рантайм userspace-программы: объекты ядра, транспорты IPC, куча и синхронизация.
//!
//! Базовый слой — svc-обёртки, futex, монотонные часы и транспорт контрактов поверх port —
//! живёт в `molecular-rt` и реэкспортируется отсюда: тот же код обслуживает стандартную
//! библиотеку Rust.

#![no_std]
#![allow(unsafe_code)]
#![feature(alloc_error_handler)]

extern crate alloc;

mod error;
mod handle;
mod heap;
mod log;
mod object;
mod ring_transport;
mod startup;
mod sync;

pub use error::{Error, Result};
pub use handle::{BorrowedHandle, OwnedHandle};
pub use log::{install_log, log_fmt, log_line};
pub(crate) use molecular_rt as svc;
pub use molecular_rt::{
    Futex, Instant, PortTransport, Primitive, SmallFutex, SmallPrimitive, futex_bucket, futex_wait,
    futex_wait_interleaved, futex_wait_without_slot, futex_wake, futex_wake_all, ipc_buffer_ptr,
};
pub use object::{
    AnonymousMapping, IrqControl, IrqLine, JoinHandle, Mapping, MemoryRegion, Port, Priority,
    Process, ReadOnlyMapping, RegionInfo, RegionKind, Reply, Resource, ServicePort, Signal, Thread,
    ThreadEntry, exit_with_sibling, spawn, spawn_owned, wait_any,
};
pub use ring_transport::RingTransport;
pub use startup::receive_startup;
pub use svc::{
    handle_close, handle_duplicate, ipc_buffer_addr, irq_ack, irq_mint, memory_allocate,
    memory_create_virtual, memory_free, memory_map, memory_region_inspect, memory_remap,
    memory_slice, memory_with_profile, port_call, port_create, port_recv, port_reply, port_send,
    process_create, process_exit, process_exit_code, process_load_image, process_resource_self,
    process_self, process_start, process_terminate, signal_create, signal_set, signal_wait_many,
    signal_wait_one, thread_create, thread_exit, thread_exit_code, thread_self, thread_terminate,
    thread_yield,
};
pub use sync::{Condvar, Mutex, MutexGuard};
pub use syscall::{MemoryAccess, MemoryProfile, Timeout, UserMemFlags};

#[global_allocator]
static PROCESS_HEAP: heap::ProcessHeap = heap::ProcessHeap::new();

#[alloc_error_handler]
fn on_alloc_error(_layout: core::alloc::Layout) -> ! {
    panic!("userspace heap: allocation failed");
}
