//! Userspace relay контракта `Log`: лог-порт доверенного владельца и пересылка
//! принятых строк его стоку.

#![no_std]

extern crate alloc;

mod kernel_sink;
mod relay;
mod thread;

pub use kernel_sink::KernelLogSink;
pub use relay::{LogSink, SourceNames};
pub use thread::LogRelayThread;
