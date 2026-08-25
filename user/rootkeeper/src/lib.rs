//! Композиция rootkeeper: план загрузки, запуск его компонента и supervisor.
#![no_std]

extern crate alloc;

mod extension;
pub mod relay;
mod system;

pub use system::{
    BootFailure, Component, Definition, LogSessions, RootSystem, SHUTDOWN_EXIT_CODE, STOP_TIMEOUT,
    Stopped, Supervised, SystemImage, supervise,
};
