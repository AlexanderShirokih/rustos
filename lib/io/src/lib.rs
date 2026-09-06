//! Вывод байт без стандартной библиотеки: сток устройства, writer поверх стока, буфер до
//! привязки устройства и типизированный доступ к MMIO.
//!
//! [`writer::Writer`] принимает готовую строку. [`byte_sink::ByteSink`] отдаёт байты
//! устройству и отвечает [`byte_sink::Pending`], когда устройство занято.
//! [`writer::BlockingWriter`] ждёт на таком стоке. [`buffered_writer::BufferedWriter`] под
//! фичей `alloc` копит вывод, пока устройство ещё не привязано.
//!
//! [`mmio::Mmio`] и [`mmio::Reg`] дают volatile-доступ к регистру устройства по его адресу.
//! [`crlf::Crlf`] переводит перевод строки в пару возврат каретки плюс перевод строки.

#![cfg_attr(not(test), no_std)]

#[cfg(feature = "alloc")]
pub mod buffered_writer;
pub mod byte_sink;
pub mod crlf;
pub mod mmio;
pub mod writer;
