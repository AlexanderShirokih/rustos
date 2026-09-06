//! Ведёт подъём ядра, общий для всех архитектур. Привязывает консоль, ставит планировщик и
//! слой syscall, обходит встроенные драйверы, запускает первый процесс образа.
//!
//! [`kmain::kmain`] принимает контекст платформы, сканер встроенных драйверов, консольный
//! writer, настройки планировщика и задачу первого запуска. Из этой функции ядро не
//! возвращается. Отказ подъёма гасит машину через [`power`].
//!
//! Платформенный слой вызывает [`kmain::kmain`] со своей реализацией `ArchContext`. Мосты
//! прерываний, системных вызовов и планировщика этот крейт сам ставит на место.

#![no_std]
#![cfg_attr(
    test,
    allow(
        clippy::std_instead_of_core,
        clippy::unimplemented,
        clippy::unwrap_in_result
    )
)]

extern crate alloc;

pub mod bootstrap;
pub mod driver_init;
pub mod init;
pub mod irq_bridge;
pub mod irq_control_adapter;
pub mod kernel_context;
pub mod kmain;
pub mod power;
pub mod sched_bridge;
pub mod scheduler_bootstrap;
pub mod syscall_bridge;
pub mod user_process;

pub use user_process::{SchedulerUserProcessLauncher, SpawnUserError, UserProcessLauncher};

#[cfg(feature = "kernel-tests")]
pub mod kernel_tests;

mod services;
