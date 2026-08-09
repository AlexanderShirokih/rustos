//! Базовый слой userspace: svc-обёртки syscall-ABI, futex, монотонные часы, куча, локальные
//! данные потока, управление потоками и транспорт ipc-контрактов поверх синхронного port.
//!
//! Крейт собирается и как обычная зависимость userland-программ (через `lib/runtime`), и как
//! зависимость стандартной библиотеки Rust. Отсюда его ограничение: зависеть он может только на
//! крейты, способные попасть в граф зависимостей `std`.

#![no_std]
#![allow(unsafe_code)]

mod alloc;
mod clock;
mod component;
mod ipc_buffer;
// Стандартная библиотека берёт futex по пути модуля: её реализации мьютекса, condvar, rwlock и
// парковки потока импортируют `crate::sys::futex::{self, ...}`.
pub mod futex;
mod lock;
pub mod log;
mod port_transport;
mod process;
pub mod random;
mod svc;
pub mod tls;

pub use alloc::{alloc, dealloc};

pub use clock::Instant;
pub use component::{adopt_startup, startup};
// Реэкспорт зависимостей, из которых стандартная библиотека берёт контракты: их объявления не
// должны переписываться руками на её стороне.
pub use component_abi;
pub use futex::{
    Futex, Primitive, SmallFutex, SmallPrimitive, futex_bucket, futex_wait, futex_wait_interleaved,
    futex_wait_without_slot, futex_wake, futex_wake_all,
};
pub use ipc;
pub use ipc_buffer::ipc_buffer_ptr;
pub use lock::{Lock, LockGuard};
pub use port_transport::PortTransport;
pub use process::{bootstrap_handle, exit, metering_handle, start};
pub use svc::{
    handle_close, handle_duplicate, ipc_buffer_addr, irq_ack, irq_mint, memory_allocate,
    memory_create_virtual, memory_free, memory_map, memory_region_inspect, memory_remap,
    memory_slice, port_call, port_create, port_recv, port_reply, port_send, process_create,
    process_exit, process_exit_code, process_load_image, process_resource_self, process_self,
    process_start, process_terminate, resource_split, signal_create, signal_set, signal_wait_many,
    signal_wait_one, thread_create, thread_exit, thread_exit_code, thread_self, thread_terminate,
    thread_yield,
};
pub use syscall;
pub use tls::{Dtor, Key, create, destroy, get, run_destructors, set};
