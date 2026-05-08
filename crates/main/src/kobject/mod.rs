//! Kernel-объекты и таблица handle'ов.
//!
//! Модуль реализует базис capability-based IPC: всё, к чему один процесс
//! может обращаться у другого (канал, событие, в перспективе -
//! поток, MMIO-регион, IRQ), представлено `KObject` и доступно строго
//! через `Handle` в [`HandleTable`] вызывающего процесса.
//!
//! # Сосуществование с `Capabilities`
//!
//! В переходный период новый код пишется через `kobject` (этот модуль),
//! а legacy сервисы продолжают жить в [`drivers_common::capabilities`].
//! Каждое место, где kernel-код всё ещё ходит через старый стор, должно
//! быть помечено грепабельным якорем `// TODO(kobject-migration)` -
//! по мере миграции эти точки удаляются.

mod api;
mod channel;
mod errors;
mod event;
mod handle;
mod handle_table;
mod koid;
mod object;
mod rights;
mod runtime;
mod wait;

pub use api::{
    channel_create, channel_read, channel_write, handle_close, handle_duplicate, install_handle,
    object_signal, object_wait_one,
};
pub use channel::{
    CHANNEL_PEER_CLOSED, CHANNEL_READABLE, ChannelEndpoint, DEFAULT_CHANNEL_CAPACITY,
    MESSAGE_INLINE_MAX, MESSAGE_MAX_HANDLES, Message,
};
pub use errors::IpcError;
pub use event::{EVENT_SIGNALED, Event};
pub use handle::{Handle, HandleId};
pub use handle_table::HandleTable;
pub use koid::Koid;
pub use object::KObject;
pub use rights::Rights;
pub use runtime::{KernelRuntime, ParkState, UserVmContext, install_runtime, runtime};
pub use wait::{SignalState, Waker};

#[cfg(test)]
mod integration_tests;
