//! Объявляет драйвер ядра и набор сервисов, которыми драйверы обмениваются: консоль,
//! контроллер прерываний, MMIO, таймер.
//!
//! Драйвер реализует трейт [`Driver`] и объявляется дескриптором [`DriverDescriptor`].
//! [`Driver::run`] получает [`BootServices`] - набор сервисов, поднятых к этому моменту.
//! Оттуда драйвер берёт нужный ему сервис и туда же кладёт свой.
//!
//! Своё устройство драйвер узнаёт по узлу [`DeviceNode`] дерева устройств. Поднятые драйверы
//! держит [`RuntimeDriverRegistry`]. `kernelspace` ведёт проходы подъёма.

#![no_std]
#![cfg_attr(test, allow(clippy::unwrap_in_result))]

extern crate alloc;

pub mod byte_sink;
pub mod crlf;
pub mod driver;
pub mod probe;
pub mod scanner;
pub mod services;
pub mod tree;

mod boot_services;
mod registry;

pub use boot_services::*;
pub use driver::*;
pub use memory::mem_flags::*;
pub use registry::*;
pub use scanner::{DriverInfo, ProbeFn};
pub use tree::{DeviceNode, DeviceTreeSource, NodeProperty};
