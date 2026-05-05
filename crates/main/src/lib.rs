//! Ядро операционной системы.
//!
//! Содержит основную логику инициализации, подсистему драйверов
//! и точку входа `kmain`.

#![no_std]
extern crate alloc;

pub mod driver_init;
pub mod irq_bridge;
pub mod kernel_context;
pub mod kmain;
pub mod kobject;
pub mod sched;

#[cfg(feature = "qemu-tests")]
pub mod qemu_tests;

mod services;
