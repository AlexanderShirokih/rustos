//! Объявляет цели, к которым процесс обращается по handle, и таблицу handle процесса.
//!
//! Всё, чем процесс пользуется через ядро, названо [`CapabilityTarget`]: сигнал, процесс,
//! поток, регион памяти, ресурс, порт, ответ на вызов, контроллер и линия прерываний.
//! Обращение к цели идёт только через [`Capability`] в [`HandleTable`] вызывающего процесса.
//!
//! Отсюда берутся операции над этими целями: установка handle в таблицу, дублирование и
//! закрытие handle, ожидание сигналов, рандеву порта, минт линии прерывания. Слой syscall,
//! планировщик и платформенный слой вызывают эти операции.
//!
//! [`install_runtime`] связывает крейт с планировщиком реализацией [`KernelRuntime`]. Подъём
//! ядра вызывает её один раз до первой операции.
//!
#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, allow(clippy::unimplemented, clippy::unwrap_in_result))]

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
    WaitManyOutcome, current_process_name, handle_close, handle_duplicate, install_handle, irq_ack,
    irq_mint, process_exit, signal_create, signal_set, signal_wait_many, signal_wait_one,
    thread_exit, thread_yield,
};
pub use errors::{IpcError, SpawnError};
pub use handle::{Capability, HandleId};
pub use handle_table::{
    DrainedHandles, HandleReservation, HandleTable, ObjectAccess, RejectedInsert,
};
pub use ipc_buffer_xfer::{release_delivered, transfer_rendezvous};
pub use irq_control::IrqControl;
pub use irq_line::IrqLine;
pub use irq_runtime::{
    InterruptsControl, IrqBindToken, IrqSink, install_interrupts_control, interrupts_control,
};
pub use port::{
    KernelIpcBuffer, Port, PortAccess, ThreadTransport, port_call, port_recv, port_send,
};
pub use process::{MeteringGrant, ProcessObject};
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
pub use target::{CapabilityTarget, OperationHold, TargetObject};
pub use thread::ThreadObject;
pub use wait::{CancelTarget, Waitable, Waker};
