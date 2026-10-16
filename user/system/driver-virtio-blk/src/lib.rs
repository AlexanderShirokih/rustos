//! Диск virtio-blk в контракте `BlockDevice`.
//!
//! [`geometry`] выводит размер блока, число блоков и предел передачи из признаков и
//! конфигурации устройства. [`Disk`] проверяет запросы клиента, выполняет их через [`Channel`]
//! и держит состояние отказа устройства.
#![no_std]

mod disk;
mod geometry;

pub use disk::{Channel, Disk, SessionBuffer};
pub use geometry::{GeometryError, geometry};
