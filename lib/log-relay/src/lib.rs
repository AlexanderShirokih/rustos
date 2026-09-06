//! Поднимает лог-порт процесса и пересылает принятые строки доверенному стоку.
//!
//! [`LogRelayThread::start`] поднимает поток пересылки. Ему передают владеющий handle порта,
//! сток [`LogSink`] и справочник имён [`SourceNames`]. Метод `session` выдаёт источнику
//! лог-сессию под его значком. По этому значку строка и получает имя источника.
//!
//! [`KernelLogSink`] пишет строки в журнал ядра bootstrap-контрактом. Отказы стока подряд
//! считаются до [`SINK_FAILURE_LIMIT`], после чего поток пересылки завершается.
//!
//! Capability из принятых кадров дальше стока не идут.

#![no_std]

extern crate alloc;

mod kernel_sink;
mod relay;
mod thread;

pub use kernel_sink::KernelLogSink;
pub use relay::{LogSink, SINK_FAILURE_LIMIT, SourceNames};
pub use thread::LogRelayThread;
