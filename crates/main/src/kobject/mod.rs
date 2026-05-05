//! Kernel-объекты и таблица handle'ов.
//!
//! Модуль реализует базис capability-based IPC: всё, к чему один процесс
//! может обращаться у другого (канал, событие, таймер, в перспективе -
//! поток, MMIO-регион, IRQ), представлено `KernelObject` за `Arc`'ом и
//! доступно строго через `Handle` в [`HandleTable`] вызывающего процесса.

mod errors;
mod handle;
mod handle_table;
mod kernel_object;
mod koid;
mod object_type;
mod rights;

pub use errors::IpcError;
pub use handle::{Handle, HandleId};
pub use handle_table::HandleTable;
pub use kernel_object::KernelObject;
pub use koid::Koid;
pub use object_type::ObjectType;
pub use rights::Rights;
