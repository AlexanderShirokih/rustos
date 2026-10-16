//! Связка библиотеки `virtio` с шиной `VirtioMmio`: открытие слота транспорта, регистры
//! слота, регион памяти, доступный устройству, и ожидание завершения команды.
#![no_std]

mod completion;
mod memory;
mod registers;
mod slot;

pub use completion::{CompletionFailure, await_completion};
pub use memory::DmaRegion;
pub use registers::{SlotFailure, SlotRegisters};
pub use slot::open_slot;
