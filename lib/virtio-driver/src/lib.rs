//! Связка библиотеки `virtio` с шиной `VirtioMmio`: открытие слота транспорта, регистры
//! слота и регион памяти, доступный устройству.
#![no_std]

mod memory;
mod registers;
mod slot;

pub use memory::DmaRegion;
pub use registers::{SlotFailure, SlotRegisters};
pub use slot::open_slot;
