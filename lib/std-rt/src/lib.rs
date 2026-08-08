//! Базовый слой userspace: svc-обёртки syscall-ABI, futex, монотонные часы, куча, локальные
//! данные потока и управление потоками.
//!
//! Крейт собирается и как обычная зависимость userland-программ (через `lib/runtime`), и как
//! зависимость стандартной библиотеки Rust. Отсюда его ограничение: зависеть он может только на
//! крейты, способные попасть в граф зависимостей `std`.

#![no_std]
#![allow(unsafe_code)]

mod alloc;
mod clock;
mod futex;
mod lock;
mod process;
mod svc;
mod tls;

pub use alloc::{alloc, dealloc};

pub use clock::Instant;
pub use futex::{
    Futex, Primitive, SmallFutex, SmallPrimitive, futex_bucket, futex_wait, futex_wait_interleaved,
    futex_wait_without_slot, futex_wake, futex_wake_all,
};
pub use lock::{Lock, LockGuard};
pub use process::{exit, metering_handle};
pub use svc::{
    handle_close, handle_duplicate, ipc_buffer_addr, irq_ack, irq_mint, memory_allocate,
    memory_create_virtual, memory_free, memory_map, memory_region_inspect, memory_remap,
    memory_slice, port_call, port_create, port_recv, port_reply, port_send, process_create,
    process_exit, process_exit_code, process_load_image, process_resource_self, process_self,
    process_start, process_terminate, resource_split, signal_create, signal_set, signal_wait_many,
    signal_wait_one, thread_create, thread_exit, thread_exit_code, thread_self, thread_terminate,
    thread_yield,
};
pub use tls::{Dtor, Key, create, destroy, get, run_destructors, set};
