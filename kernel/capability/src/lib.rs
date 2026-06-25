//! capability targets и таблица handle'ов.
//!
//! Модуль реализует базис capability-based IPC: всё, к чему один процесс
//! может обращаться у другого (канал, событие, в перспективе -
//! поток, MMIO-регион, IRQ), представлено `CapabilityTarget` и доступно строго
//! через `Capability` в [`HandleTable`] вызывающего процесса.
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
mod irq_control;
mod irq_line;
mod irq_runtime;
mod port;
mod process;
mod reply;
mod resource;
mod rev_node;
mod rights;
mod runtime;
mod signal;
mod spawn;
mod target;
mod termination;
mod thread;
mod wait;

pub use api::{
    WaitManyOutcome, handle_close, handle_duplicate, install_handle, irq_ack, irq_mint,
    signal_create, signal_set, signal_wait_many, signal_wait_one, thread_exit,
};
pub use errors::{IpcError, SpawnError};
pub use handle::{Capability, HandleId};
pub use handle_table::{HandleReservation, HandleTable};
pub use ipc_buffer_xfer::transfer_rendezvous;
pub use irq_control::IrqControl;
pub use irq_line::IrqLine;
pub use irq_runtime::{
    InterruptsControl, IrqBindToken, IrqSink, install_interrupts_control, interrupts_control,
};
pub use port::{KernelIpcBuffer, Port, ThreadTransport, port_call, port_recv, port_send};
pub use process::ProcessObject;
pub use reply::Reply;
pub use resource::{Resource, ResourceBudgetRefund};
pub use rev_node::RevocationHook;
pub use rights::default_rights_for;
pub use runtime::{KernelRuntime, ParkState, UserThreadEntry, WaitToken, install_runtime, runtime};
pub use signal::{SIGNALED, Signal, WakeCount};
pub use spawn::{
    LoadImageError, StartProcessError, UserImageInstall, UserSegmentInstall, UserStartSpec,
};
pub use syscall::Rights;
pub use target::CapabilityTarget;
pub use thread::ThreadObject;
pub use wait::{CancelTarget, Waitable, Waker};
