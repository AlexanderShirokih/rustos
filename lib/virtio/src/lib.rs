//! Обмен драйвера с устройством virtio через транспорт MMIO версии 2 и split-очередь.
//!
//! Драйвер реализует [`Registers`] над окном транспорта и [`DeviceMemory`] над непрерывным
//! регионом, доступным устройству. [`Transport::probe`] проверяет окно без записи,
//! [`Transport::negotiate`] сбрасывает устройство и согласует признаки, [`SplitQueue::new`]
//! передаёт устройству очередь, [`Transport::start`] выставляет DRIVER_OK.
//!
//! [`SplitQueue::submit`] отправляет команду цепочкой "запрос, ответ",
//! [`SplitQueue::take_used`] забирает завершённую. Ожидание прерывания остаётся драйверу.
//! Формат команд GPU 2D даёт модуль [`gpu`].
#![cfg_attr(not(test), no_std)]

mod access;
pub mod gpu;
mod queue;
#[cfg(test)]
mod testing;
mod transport;

pub use access::{DeviceMemory, OutOfRange, Registers};
pub use queue::{
    Buffer, Completion, QUEUE_REGION_BYTES, QUEUE_SIZE_MAX, QueueError, SplitQueue, Token,
};
pub use transport::{DeviceType, Transport, TransportError, register};
