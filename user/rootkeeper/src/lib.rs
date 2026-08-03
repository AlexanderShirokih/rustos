//! Владение managed-компонентами верхнего уровня rootkeeper.
#![no_std]

mod managed;

pub use managed::{
    BoundedStartupTransport, ComponentId, ManagedComponent, ManagedFailure, ManagedStartupError,
    ManagedStatus, RunningComponent,
};
