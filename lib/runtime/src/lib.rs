//! Платформенный слой процесса userspace: куча, futex и синхронизация, локальные данные
//! потока, запуск и сон потоков, монотонные часы и снимок часов реального времени, стартовый
//! handle, неявные импорты компонента и журнал.
//!
//! Каждый из этих механизмов держит состояние процесса и существует в процессе в единственном
//! экземпляре. Программа со стандартной библиотекой получает их через неё, программа без неё -
//! прямой зависимостью на этот крейт, и глобальный аллокатор сама объявляет. Сторона userspace
//! у syscall-ABI лежит в `syscall-user` и реэкспортируется отсюда. Состояния она не хранит и
//! дублируется свободно.

#![no_std]
#![allow(unsafe_code)]

extern crate alloc;

mod atomic_handle;
mod clock;
// Стандартная библиотека берёт futex по пути модуля. Её реализации мьютекса, condvar, rwlock и
// парковки потока импортируют `crate::sys::futex::{self, ...}`.
pub mod futex;
mod heap;
mod imports;
pub mod log;
mod process;
pub mod random;
pub mod sync;
pub mod thread;
pub mod tls;

pub(crate) use atomic_handle::restore;
pub use clock::{Instant, install_realtime, realtime};
pub use futex::{
    Futex, futex_bucket, futex_wait, futex_wait_interleaved, futex_wait_without_slot, futex_wake,
    futex_wake_all,
};
pub use heap::{ProcessHeap, alloc, dealloc};
pub use imports::start_component;
pub use log::{fmt as log_fmt, install as install_log, line as log_line};
pub use process::{
    ExitCode, FAILED_EXIT_CODE, SUCCESS_EXIT_CODE, Termination, exit, finish, metering_handle,
    panic, set_panic_hook, start, take_bootstrap,
};
pub use sync::{Condvar, Mutex, MutexGuard, RawCondvar, RawMutex};
pub use syscall::{
    Handle, MemoryAccess, MemoryProfile, ObjectKind, Priority, Timeout, UserMemFlags, kind,
};
pub(crate) use syscall_user as svc;
pub use syscall_user::{
    Delivery, IssuedHandle, PortTransport, handle_close, handle_duplicate, ipc_buffer_addr,
    ipc_buffer_ptr, irq_ack, irq_mint, memory_allocate, memory_create_virtual, memory_free,
    memory_map, memory_region_inspect, memory_remap, memory_slice, pending_take, port_call,
    port_call_async, port_create, port_recv, port_reply, port_send, port_send_async,
    process_create, process_exit, process_exit_code, process_load_image, process_resource_self,
    process_self, process_start, process_terminate, signal_create, signal_set, signal_wait_many,
    signal_wait_one, thread_create, thread_exit, thread_exit_code, thread_self, thread_terminate,
    thread_yield,
};
pub use thread::{JoinHandle, sleep, spawn, spawn_bound, spawn_owned};
