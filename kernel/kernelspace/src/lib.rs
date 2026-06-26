//! Ядро операционной системы.
//!
//! Содержит основную логику инициализации, подсистему драйверов
//! и точку входа `kmain`.

#![no_std]
extern crate alloc;

pub mod bootstrap;
pub mod driver_init;
pub mod init;
pub mod irq_bridge;
pub mod irq_control_adapter;
pub mod kernel_context;
pub mod kmain;
pub mod power;
pub mod scheduler_bootstrap;
pub mod syscall_bridge;
pub mod user_process;

pub use user_process::{SchedulerUserProcessLauncher, SpawnUserError, UserProcessLauncher};

#[cfg(feature = "kernel-tests")]
pub mod kernel_tests;

mod services;
