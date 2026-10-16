//! Обмен драйвера с устройством virtio через транспорт MMIO версии 2 и split-очередь.
//!
//! Драйвер реализует [`Registers`] над окном транспорта и [`DeviceMemory`] над непрерывным
//! регионом, доступным устройству.
//!
//! [`SplitQueue::submit`] отправляет цепочку из буферов, которые устройство читает, и буферов,
//! которые оно пишет, [`SplitQueue::take_used`] забирает завершённую цепочку. Ожидание
//! прерывания остаётся драйверу.
//! Формат команд GPU 2D даёт модуль [`gpu`], формат конфигурации и событий устройства
//! ввода - модуль [`input`], формат запросов блочного устройства - модуль [`block`].
#![cfg_attr(not(test), no_std)]

mod access;
pub mod block;
pub mod gpu;
pub mod input;
mod queue;
#[cfg(test)]
mod testing;
mod transport;

pub use access::{AccessWidth, DeviceMemory, OutOfRange, Registers};
pub use queue::{
    Buffer, Completion, QUEUE_REGION_BYTES, QUEUE_SIZE_MAX, QueueError, SplitQueue, Token,
};
pub use transport::{DeviceType, Transport, TransportError, register};
