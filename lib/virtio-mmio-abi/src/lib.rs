//! Контракты шины транспортов virtio-mmio: [`virtio_mmio`] выдаёт слот транспорта по типу
//! устройства, [`virtio_slot`] передаёт регистры, конфигурацию устройства и линию прерывания
//! транспорта слота.
//!
//! Слот занят, пока жив клиент его сессии. После ухода клиента шина сбрасывает устройство
//! записью 0 в регистр Status и снова выдаёт транспорт, когда Status читается 0.

#![cfg_attr(not(test), no_std)]

mod error;
mod protocol;
mod width;

pub use error::{SlotError, VirtioMmioError};
pub use protocol::{virtio_mmio, virtio_slot};
pub use width::AccessWidth;
