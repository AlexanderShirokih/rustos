//! Объявляет макросы записи в журнал ядра - [`fatal!`], [`error!`], [`warn!`], [`info!`],
//! [`debug!`] - и сток, которому эти макросы отдают строку.
//!
//! [`set_stdout`] ставит стоком реализацию `io::writer::Writer`, привязанную на подъёме ядра.
//! Путь паники пишет в отдельный сток, который ставит [`set_panic_stdout`]. Лок обычного
//! стока запись из паники не берёт.
//!
//! Строка консоли начинается временем от источника [`set_clock`]. [`attach_ring`] подключает
//! кольцо лога, куда та же строка пишется записью. До подключения записи копятся в куче,
//! [`detach_ring`] от кольца отказывается и освобождает их.
//! Строку userspace пишет [`log_user`].
//!
//! Важность строки названа [`Level`]. [`file_tag`] достаёт из пути имя файла-источника.

#![no_std]
extern crate alloc;

mod buffered_writer;
mod level;
mod macros;
mod panic_sink;
mod stdout;

pub use buffered_writer::BufferedWriter;
pub use level::Level;
pub use panic_sink::{panic_logf, panic_printf, set_panic_stdout};
pub use stdout::{
    Clock, attach_ring, detach_ring, file_tag, log_user, logf_tagged, set_clock, set_stdout,
};
