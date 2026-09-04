//! Платформенный слой процесса userspace: куча, futex и синхронизация, локальные данные
//! потока, запуск потоков, монотонные часы, стартовый handle и журнал.
//!
//! Каждый из этих механизмов держит состояние процесса и существует в процессе в единственном
//! экземпляре. Программа со стандартной библиотекой получает их через неё, программа без неё -
//! прямой зависимостью на этот крейт, и глобальный аллокатор объявляет сама. Сторона userspace
//! у syscall-ABI лежит в `syscall-user` и реэкспортируется отсюда. Состояния она не несёт и
//! дублируется свободно.

#![no_std]
#![allow(unsafe_code)]

extern crate alloc;

mod clock;
// Стандартная библиотека берёт futex по пути модуля: её реализации мьютекса, condvar, rwlock и
// парковки потока импортируют `crate::sys::futex::{self, ...}`.
pub mod futex;
mod heap;
pub mod log;
mod process;
pub mod random;
mod startup;
pub mod sync;
pub mod thread;
pub mod tls;

pub use clock::Instant;
pub use futex::{
    Futex, futex_bucket, futex_wait, futex_wait_interleaved, futex_wait_without_slot, futex_wake,
    futex_wake_all,
};
pub use heap::{ProcessHeap, alloc, dealloc};
pub use log::{fmt as log_fmt, install as install_log, line as log_line};
pub use process::{exit, metering_handle, start, take_bootstrap};
pub use startup::receive_startup;
pub use sync::{Condvar, Mutex, MutexGuard};
pub use syscall::{Handle, MemoryAccess, MemoryProfile, Priority, Timeout, UserMemFlags};
pub(crate) use syscall_user as svc;
pub use syscall_user::{
    PortTransport, handle_close, handle_duplicate, ipc_buffer_addr, ipc_buffer_ptr, irq_ack,
    irq_mint, memory_allocate, memory_create_virtual, memory_free, memory_map,
    memory_region_inspect, memory_remap, memory_slice, memory_with_profile, port_call, port_create,
    port_recv, port_reply, port_send, process_create, process_exit, process_exit_code,
    process_load_image, process_resource_self, process_self, process_start, process_terminate,
    signal_create, signal_set, signal_wait_many, signal_wait_one, thread_create, thread_exit,
    thread_exit_code, thread_self, thread_terminate, thread_yield,
};
pub use thread::{JoinHandle, exit_with_sibling, spawn, spawn_owned};
