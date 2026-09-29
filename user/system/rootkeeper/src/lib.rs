//! Поднимает композицию образа и служит родителем её записей.
//!
//! [`run`] принимает handle bootstrap-порта от ядра и возвращает код завершения процесса
//! [`Stopped`].
#![no_std]

extern crate alloc;

pub mod relay;
mod system;

pub use system::{
    BUDGET_RETURN_TIMEOUT, BootFailure, BudgetFailure, ImporterRefusal, ImporterRequirement,
    RootSystem, Stopped, SystemImage, run, turn,
};
