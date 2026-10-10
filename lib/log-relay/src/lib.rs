//! Поднимает лог-порт процесса и пересылает принятые строки доверенному стоку.
//!
//! [`LogRelayThread::start`] поднимает поток пересылки. Ему передают владеющий handle порта,
//! сток [`LogSink`] и справочник имён [`SourceNames`]. Метод `session` выдаёт источнику
//! лог-сессию под его значком. По этому значку строка и получает имя источника.
//!
//! [`KernelLogSink`] пишет строки в журнал ядра bootstrap-контрактом. Отказы стока подряд
//! считаются до [`SINK_FAILURE_LIMIT`], после чего поток пересылки завершается.
//!
//! [`Journal`] хранит историю строк и копирует каждую строку в кольца подписчиков,
//! [`JournalSink`] пишет в него строки relay перед следующим стоком.
//!
//! Capability из принятых кадров дальше стока не идут.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod journal;
mod kernel_sink;
mod relay;
mod thread;

pub use journal::{JOURNAL_REGION_BYTES, Journal, JournalLine, JournalRing, JournalSink};
pub use kernel_sink::KernelLogSink;
pub use relay::{LogSink, SINK_FAILURE_LIMIT, SourceNames};
pub use thread::LogRelayThread;
