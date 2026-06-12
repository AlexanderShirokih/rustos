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
mod channel;
mod errors;
mod event;
mod handle;
mod handle_table;
mod koid;
mod mailbox;
mod object;
mod physical_resource;
mod process;
mod rights;
mod runtime;
mod spawn;
mod termination;
mod thread;
mod wait;

pub use api::{
    WaitManyOutcome, channel_create, channel_read, channel_write, create_empty_process,
    create_user_thread, handle_close, handle_duplicate, install_handle, load_user_image_into,
    mailbox_cancel, mailbox_create, mailbox_queue, mailbox_wait, mailbox_wait_async, object_signal,
    object_wait_many, object_wait_one, start_user_process, terminate_process, terminate_thread,
    thread_exit,
};
pub use channel::{
    CHANNEL_PEER_CLOSED, CHANNEL_READABLE, CHANNEL_WRITABLE, Channel, DEFAULT_CHANNEL_CAPACITY,
    MESSAGE_INLINE_MAX, MESSAGE_MAX_HANDLES, Message,
};
pub use errors::{IpcError, SpawnError};
pub use event::{EVENT_SIGNALED, Event};
pub use handle::{Handle, HandleId};
pub use handle_table::{HandleReservation, HandleTable};
pub use koid::Koid;
pub use mailbox::{
    AsyncMode, MAILBOX_PACKET_SIZE, MAILBOX_PAYLOAD_SIZE, MAILBOX_QUEUE_CAPACITY, MAILBOX_READABLE,
    Mailbox, MailboxPacket, MailboxPacketKind,
};
pub use object::KObject;
pub use physical_resource::PhysicalResource;
pub use process::{PROCESS_TERMINATED, ProcessObject};
pub use rights::Rights;
pub use runtime::{KernelRuntime, ParkState, UserThreadEntry, WaitToken, install_runtime, runtime};
pub use spawn::{
    LoadImageError, StartProcessError, UserImageInstall, UserSegmentInstall, UserStartSpec,
};
pub use thread::{THREAD_TERMINATED, ThreadObject};
pub use wait::{CancelTarget, SignalSource, SignalState, Waker};

#[cfg(test)]
mod integration_tests;
