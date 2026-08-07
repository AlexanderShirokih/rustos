//! Композиция rootkeeper: план загрузки, запуск его компонента и supervisor.
#![no_std]

pub mod relay;
mod system;

pub use system::{
    BootFailure, RootSystem, STOP_TIMEOUT, Stopped, Supervised, SystemImage, supervise,
};
