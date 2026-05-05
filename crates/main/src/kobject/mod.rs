//! Kernel-объекты и таблица handle'ов.
//!
//! Модуль реализует базис capability-based IPC: всё, к чему один процесс
//! может обращаться у другого (канал, событие, таймер, в перспективе -
//! поток, MMIO-регион, IRQ), представлено `KernelObject` за `Arc`'ом и
//! доступно строго через `Handle` в [`HandleTable`] вызывающего процесса.
//!
//! # Сосуществование с `Capabilities`
//!
//! В переходный период новый код пишется через `kobject` (этот модуль),
//! а legacy сервисы продолжают жить в [`drivers_common::capabilities`].
//! Каждое место, где kernel-код всё ещё ходит через старый стор, должно
//! быть помечено грепабельным якорем `// TODO(kobject-migration)` -
//! по мере миграции эти точки удаляются.

mod channel;
mod errors;
mod event;
mod handle;
mod handle_table;
mod kernel_object;
mod koid;
mod object_type;
mod rights;
mod wait;

pub use channel::{
    CHANNEL_PEER_CLOSED, CHANNEL_READABLE, ChannelEndpoint, DEFAULT_CHANNEL_CAPACITY,
    MESSAGE_INLINE_MAX, MESSAGE_MAX_HANDLES, Message,
};
pub use errors::IpcError;
pub use event::{EVENT_SIGNALED, Event};
pub use handle::{Handle, HandleId};
pub use handle_table::HandleTable;
pub use kernel_object::KernelObject;
pub use koid::Koid;
pub use object_type::ObjectType;
pub use rights::Rights;
pub use wait::{SignalState, Waker};

#[cfg(test)]
mod integration_tests;
