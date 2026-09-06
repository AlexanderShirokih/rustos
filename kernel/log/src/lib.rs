//! Объявляет макросы записи в журнал ядра - [`fatal!`], [`error!`], [`warn!`], [`info!`],
//! [`debug!`] - и сток, которому эти макросы отдают строку.
//!
//! [`set_stdout`] ставит стоком реализацию `io::writer::Writer`, привязанную на подъёме ядра.
//! Путь паники пишет в отдельный сток, который ставит [`set_panic_stdout`]. Лок обычного
//! стока запись из паники не берёт.
//!
//! Важность строки названа [`Level`]. [`file_tag`] достаёт из пути имя файла-источника.

#![no_std]
extern crate alloc;

mod level;
mod macros;
mod panic_sink;
mod stdout;

pub use level::Level;
pub use panic_sink::{panic_logf, panic_printf, set_panic_stdout};
pub use stdout::{file_tag, logf, logf_tagged, printf, set_stdout};
