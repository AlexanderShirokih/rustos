//! Kernel-объекты и таблица handle'ов.
//!
//! Модуль реализует базис capability-based IPC: всё, к чему один процесс
//! может обращаться у другого (канал, событие, в перспективе -
//! поток, MMIO-регион, IRQ), представлено `KObject` и доступно строго
//! через `Handle` в [`HandleTable`] вызывающего процесса.
//!
#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod api;
mod errors;
mod handle;
mod handle_table;
mod ipc_buffer_xfer;
mod koid;
mod object;
mod port;
mod process;
mod reply;
mod resource;
mod rights;
mod runtime;
mod signal;
mod spawn;
mod termination;
mod thread;
mod wait;

pub use api::{
    WaitManyOutcome, create_empty_process, create_user_thread, handle_close, handle_duplicate,
    install_handle, load_user_image_into, process_termination_signal, signal_create, signal_set,
    signal_wait_many, signal_wait_one, start_user_process, terminate_process, terminate_thread,
    thread_exit, thread_termination_signal,
};
pub use errors::{IpcError, SpawnError};
pub use handle::{Handle, HandleId};
pub use handle_table::{HandleReservation, HandleTable};
pub use ipc_buffer_xfer::transfer_rendezvous;
pub use koid::Koid;
pub use object::KObject;
pub use port::{
    BufferAccess, KernelIpcBuffer, OutcomeSlot, Port, RendezvousOutcome, ThreadTransport,
    WaiterKind, port_call, port_recv, port_send,
};
pub use process::ProcessObject;
pub use reply::{Reply, ReplySlot};
pub use resource::Resource;
pub use rights::Rights;
pub use runtime::{KernelRuntime, ParkState, UserThreadEntry, WaitToken, install_runtime, runtime};
pub use signal::{SIGNALED, Signal};
pub use spawn::{
    LoadImageError, StartProcessError, UserImageInstall, UserSegmentInstall, UserStartSpec,
};
pub use thread::ThreadObject;
pub use wait::{CancelTarget, Waker};
