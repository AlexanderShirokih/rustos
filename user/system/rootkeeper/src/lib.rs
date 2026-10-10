//! Поднимает композицию образа и служит родителем её записей.
//!
//! [`run`] принимает handle bootstrap-порта от ядра и возвращает код завершения процесса
//! [`Stopped`].
#![no_std]

extern crate alloc;

mod boot;
mod component;
pub mod relay;
mod system;

pub use boot::{BootFailure, run};
pub use component::{Component, ImporterRequirement};
pub use system::{
    AddFailure, BUDGET_RETURN_TIMEOUT, BudgetFailure, ImporterRefusal, RecordState, RootSystem,
    SEARCH_LIMITS, SearchLimits, Stopped, SystemImage, turn,
};
