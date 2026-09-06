//! Поднимает композицию образа и держит её до завершения критического компонента.
//!
//! [`supervise`] принимает handle bootstrap-порта, который программа получает от ядра, и
//! возвращает [`Stopped`] - код, которым ядро гасит машину. Отказ подъёма назван
//! [`BootFailure`], нехватка ресурса композиции - [`BudgetFailure`], несобранный слот -
//! [`SlotFailure`].
//!
//! Лог-сессии компонентов сводит модуль [`relay`]: сток журнала остаётся в этом процессе,
//! компоненту достаётся сессия под своим значком.
#![no_std]

extern crate alloc;

mod extension;
pub mod relay;
mod system;

pub use system::{
    BootFailure, BudgetFailure, Component, Definition, LogSessions, RootSystem, SHUTDOWN_EXIT_CODE,
    STOP_TIMEOUT, SlotFailure, Stopped, Supervised, SystemImage, supervise,
};
