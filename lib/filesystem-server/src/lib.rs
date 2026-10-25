//! Обслуживание экспорта `FileSystem` и сессий файловой системы.

#![no_std]

extern crate alloc;

mod control;
mod error;
mod io;
mod server;
mod service;
mod session;
mod store;
mod volume;

pub use error::{Error, Result};
pub use server::{serve, serve_volumes};
