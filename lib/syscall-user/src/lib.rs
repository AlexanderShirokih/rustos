//! Сторона userspace у syscall-ABI: обёртки системных вызовов, доступ к IPC-буферу потока и
//! транспорт ipc-контрактов поверх синхронного port.
//!
//! Крейт не несёт состояния процесса, и две его сборки в одном процессе равнозначны. Он
//! линкуется и внутрь стандартной библиотеки, и в программу, взявшую его прямой зависимостью.

#![no_std]
#![allow(unsafe_code)]

mod delivery;
mod ipc_buffer;
mod port_transport;
mod svc;

pub use delivery::{Delivery, ReceivedHandle};
pub use ipc;
pub use ipc_buffer::ipc_buffer_ptr;
pub use port_transport::PortTransport;
pub use svc::{
    handle_close, handle_duplicate, ipc_buffer_addr, irq_ack, irq_mint, memory_allocate,
    memory_create_virtual, memory_free, memory_map, memory_region_inspect, memory_remap,
    memory_slice, memory_with_profile, port_call, port_create, port_recv, port_reply, port_send,
    process_create, process_exit, process_exit_code, process_load_image, process_resource_self,
    process_self, process_start, process_terminate, resource_split, signal_create, signal_set,
    signal_wait_many, signal_wait_one, thread_create, thread_exit, thread_exit_code, thread_self,
    thread_terminate, thread_yield,
};
pub use syscall;
