//! Примитив managed-запуска: владение экземпляром компонента, его резервом
//! бюджета и одноразовым startup-кадром.

#![no_std]

mod component;
mod roots;

pub use component::{
    BoundedStartupTransport, ManagedComponent, ManagedFailure, ManagedStartupError, ManagedStatus,
    RunningComponent,
};
pub use roots::{ComponentRoots, ComponentSlots, launch_component};
