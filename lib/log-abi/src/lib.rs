//! Контракт `Log` для односторонней записи строки в системный журнал и сборка такой строки
//! на стороне пишущего.
//!
//! Клиента и сервис даёт модуль [`log`], его единственная операция `write` принимает текст
//! строки. Источник строки сервер контракта выводит из значка сессии.
//! [`log::Client::write_str`] режет сообщение длиннее кадра на несколько вызовов.
//!
//! [`RingWriter`] и [`RingReader`] пишут и читают кольцо лога ядра, [`LogRing`] выдаёт его
//! регион и сигнал. Модуль [`journal`] выдаёт подписчику это кольцо.
//!
//! [`LogBuffer`] копит строку на стеке до отправки, [`LineAssembler`] собирает строки из
//! потока фрагментов, [`chunks`] режет текст по границе символа. [`Utf8Assembler`] собирает
//! текст из байтового потока вывода.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod buffer;
mod journal_session;
mod level;
mod line;
mod log_ring;
mod utf8;
mod write;

pub use buffer::{LogBuffer, chunks};
pub use journal_session::{JournalError, journal};
pub use level::Level;
pub use line::LineAssembler;
pub use log_ring::{
    Entry, LogRing, RECORD_MAX, Record, RecordBuf, RingReader, RingWriter, WordLoad, ring_words,
};
pub use utf8::Utf8Assembler;

/// Граница доверенного источника строки. Его назначает сервер контракта по значку сессии.
pub const LOG_SOURCE_MAX: usize = 64;

/// Граница строки лога: остаток кадра за доверенным источником.
pub const LOG_MESSAGE_MAX: usize =
    ipc::wire::FIELD_DATA_MAX - LOG_SOURCE_MAX - ipc::wire::FIELD_OVERHEAD;

const TERMINATOR_SIZE: usize = 1;

const _: () = assert!(
    TERMINATOR_SIZE
        + ipc::wire::FIELD_OVERHEAD
        + LOG_SOURCE_MAX
        + ipc::wire::FIELD_OVERHEAD
        + LOG_MESSAGE_MAX
        <= ipc::wire::BODY_MAX,
    "log frame with a composed source exceeds the message budget"
);

ipc::protocol! {
    /// Запись строки в системный журнал.
    pub mod log {
        name = "Log";

        /// Пишет строку в системный журнал. Строку длиннее кадра клиентская обёртка
        /// разбивает на несколько вызовов. Полномочий сверх записи строки capability на
        /// этот контракт не даёт.
        cast fn write(message: &str);
    }
}

#[cfg(test)]
mod tests {
    use core::cell::RefCell;
    use std::string::String;

    use super::{LOG_MESSAGE_MAX, log};

    #[derive(Default)]
    pub(crate) struct Sink {
        pub(crate) last: RefCell<Option<String>>,
    }

    impl<F: ipc::ReceivedResources> log::Service<F> for Sink {
        fn write(&self, _frame: &F, message: &str) {
            *self.last.borrow_mut() = Some(String::from(message));
        }
    }

    #[test]
    fn write_round_trip() {
        let (client_end, server_end) = ipc_test::pair();
        let client = log::Client::new(client_end);
        client.write("log line").wait().expect("log write");

        let sink = Sink::default();
        server_end
            .serve(|frame| log::dispatch(&sink, frame))
            .expect("dispatch ok");
        assert_eq!(sink.last.borrow().as_deref(), Some("log line"));
    }

    #[test]
    fn message_at_the_bound_round_trips() {
        let (client_end, server_end) = ipc_test::pair();
        let client = log::Client::new(client_end);
        let message = "m".repeat(LOG_MESSAGE_MAX);
        client.write(&message).wait().expect("log write");

        let sink = Sink::default();
        server_end
            .serve(|frame| log::dispatch(&sink, frame))
            .expect("dispatch ok");
        assert_eq!(
            sink.last.into_inner().expect("logged").len(),
            LOG_MESSAGE_MAX
        );
    }
}
