//! Log-контракт: односторонняя запись строки в системный журнал.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod buffer;
mod write;

pub use buffer::{LogBuffer, chunks};

/// Граница локального тега источника, назначаемого клиентом.
pub const LOG_TAG_MAX: usize = 32;

/// Граница доверенного источника строки: его составляет сервер контракта из
/// значка сессии и локального тега клиента.
pub const LOG_SOURCE_MAX: usize = 64;

/// Граница строки лога: остаток кадра за доверенным источником.
pub const LOG_MESSAGE_MAX: usize =
    ipc::wire::FIELD_DATA_MAX - LOG_SOURCE_MAX - ipc::wire::FIELD_OVERHEAD;

// Байт-терминатор тела кадра: его бюджет учитывается наравне с полями.
const TERMINATOR_SIZE: usize = 1;

// Кадр с доверенным источником вместо локального тега укладывается в бюджет
// сообщения целиком.
const _: () = assert!(
    TERMINATOR_SIZE
        + ipc::wire::FIELD_OVERHEAD
        + LOG_SOURCE_MAX
        + ipc::wire::FIELD_OVERHEAD
        + LOG_MESSAGE_MAX
        <= ipc::wire::BODY_MAX,
    "log frame with a composed source exceeds the message budget"
);

#[ipc::protocol(name = "Log")]
pub trait Log {
    /// Пишет строку в системный журнал. `tag` - локальный тег источника внутри
    /// процесса-клиента, пустой тег - строка без него. Строку длиннее кадра
    /// клиентская обёртка разбивает на несколько вызовов. Полномочий сверх
    /// записи строки capability на этот контракт не даёт.
    #[cast]
    fn write(
        &self,
        tag: ipc::wire::Str<{ LOG_TAG_MAX }>,
        message: ipc::wire::Str<{ LOG_MESSAGE_MAX }>,
    );
}

#[cfg(test)]
mod tests {
    use std::{format, string::String};

    use ipc::wire::Str;
    use ipc_test::MockEnd;

    use super::{LOG_MESSAGE_MAX, LOG_TAG_MAX, LogClient, LogService, dispatch_log};

    #[derive(Default)]
    pub(crate) struct Sink {
        pub(crate) last: Option<String>,
    }

    impl LogService for Sink {
        fn write(&mut self, tag: Str<'_, { LOG_TAG_MAX }>, message: Str<'_, { LOG_MESSAGE_MAX }>) {
            self.last = Some(format!("[{}] {}", tag.as_str(), message.as_str()));
        }
    }

    #[test]
    fn write_round_trip() {
        let (client_end, server_end) = MockEnd::pair();
        let client = LogClient::new(client_end);
        client
            .write(
                Str::new("relay").expect("tag within bound"),
                Str::new("log line").expect("message within bound"),
            )
            .expect("log write");

        let mut sink = Sink::default();
        dispatch_log(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(sink.last.as_deref(), Some("[relay] log line"));
    }

    #[test]
    fn empty_tag_round_trips() {
        let (client_end, server_end) = MockEnd::pair();
        let client = LogClient::new(client_end);
        client
            .write(
                Str::new("").expect("empty tag within bound"),
                Str::new("untagged").expect("message within bound"),
            )
            .expect("log write");

        let mut sink = Sink::default();
        dispatch_log(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(sink.last.as_deref(), Some("[] untagged"));
    }

    #[test]
    fn message_at_the_bound_round_trips() {
        let (client_end, server_end) = MockEnd::pair();
        let client = LogClient::new(client_end);
        let message = "m".repeat(LOG_MESSAGE_MAX);
        client
            .write(
                Str::new("relay").expect("tag within bound"),
                Str::new(&message).expect("message within bound"),
            )
            .expect("log write");

        let mut sink = Sink::default();
        dispatch_log(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(sink.last.expect("logged").len(), 8 + LOG_MESSAGE_MAX);
    }
}
