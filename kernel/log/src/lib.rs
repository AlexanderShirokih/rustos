//! Крейт логирования ядра.
//!
//! Предоставляет макросы для вывода сообщений с уровнями логирования.

#![no_std]
extern crate alloc;

mod level;
mod macros;
mod panic_sink;
mod stdout;

pub use level::Level;
pub use panic_sink::{panic_logf, panic_printf, set_panic_stdout};
pub use stdout::{file_tag, logf, logf_tagged, printf, set_stdout};
