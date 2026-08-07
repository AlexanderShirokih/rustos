//! Пересылка строк лога доверенному стоку: источник строки выводится из значка
//! сессии, capability из принятых кадров не переносятся дальше.

use core::fmt::Write as _;

use ipc::{Transport as _, wire::IpcError};
use log_abi::{LOG_MESSAGE_MAX, LOG_SOURCE_MAX, LOG_TAG_MAX, LogBuffer, LogService, dispatch_log};
use runtime::{OwnedHandle, PortTransport, handle_close};
use syscall::Timeout;

const RELAY_SOURCE: &str = "log-relay";

const RESOURCE_EXHAUSTED_MESSAGE: &str = "frame dropped: resources exhausted";

/// Предел подряд идущих отказов стока: на нём relay уходит из своего цикла.
/// Успешная пересылка обнуляет счёт.
pub const SINK_FAILURE_LIMIT: u32 = 8;

const ESCAPED_CHARACTER_MAX: usize = 10;

const _: () = assert!(
    LOG_MESSAGE_MAX >= ESCAPED_CHARACTER_MAX,
    "a log message holds the widest escaped scalar, `\\u{{10ffff}}`"
);

/// Сток строк лога: доверенный канал, недоступный клиентам лог-порта.
pub trait LogSink {
    /// Пишет нормализованный bounded-кусок строки источника `source` в журнал.
    /// Аргументы не содержат физических переводов строк и Unicode controls.
    fn write(&mut self, source: &str, message: &str) -> Result<(), IpcError>;
}

/// Имя подсистемы по значку её лог-сессии. `None` - значок попадает в журнал
/// числовой формой.
pub type SourceNames = fn(u64) -> Option<&'static str>;

pub(crate) struct LogRelay<S: LogSink> {
    receiver: OwnedHandle,
    names: SourceNames,
    sink: S,
}

impl<S: LogSink> LogRelay<S> {
    pub(crate) fn new(receiver: OwnedHandle, names: SourceNames, sink: S) -> Self {
        Self {
            receiver,
            names,
            sink,
        }
    }

    pub(crate) fn serve(&mut self) {
        let reason = self.receive_loop();
        let _ = self.sink.write(RELAY_SOURCE, reason);
    }

    fn receive_loop(&mut self) -> &'static str {
        let transport = PortTransport::server(self.receiver.borrow());
        let mut failures = 0;
        loop {
            match transport.wait_readable(Timeout::INFINITE) {
                Ok(()) => {}
                // Ни один исход кадра не несёт: состояния сессий relay не держит, а
                // бессрочный приём срока не имеет и пробуждение без кадра цикл не завершает.
                Err(IpcError::SenderGone | IpcError::Timeout) => continue,
                // Исчерпание относится к одному кадру. Буфер приёма при этом
                // не содержит доставленного сообщения.
                Err(IpcError::ResourceExhausted) => {
                    let _ = self.sink.write(RELAY_SOURCE, RESOURCE_EXHAUSTED_MESSAGE);
                    continue;
                }
                Err(IpcError::PeerClosed) => return "stopped: log port closed",
                Err(_) => return "stopped: log port receive failed",
            }

            // Значок и capability кадра читаются до пересылки: исходящий IPC
            // перезаписывает те же поля IPC-буфера потока.
            let source = source_of(self.names, transport.peer_badge());
            discard_staged_caps(&transport);

            let mut forwarding = Forwarding {
                sink: &mut self.sink,
                source: source.as_str(),
                forwarded: None,
            };
            let _ = dispatch_log(&mut forwarding, &transport);
            let forwarded = forwarding.forwarded;
            // Закрытие неотправленного ответа будит припаркованного вызывателя.
            transport.cancel_reply();

            match forwarded {
                // Кадр вне контракта до стока не доходит и следа в журнале не
                // оставляет: их объём задаёт клиент, а не relay.
                None => {}
                Some(Ok(())) => failures = 0,
                Some(Err(_)) => {
                    failures += 1;
                    if failures >= SINK_FAILURE_LIMIT {
                        return "stopped: sink refused every line in a row";
                    }
                }
            }
        }
    }
}

struct Forwarding<'a, S: LogSink> {
    sink: &'a mut S,
    source: &'a str,
    forwarded: Option<Result<(), IpcError>>,
}

impl<S: LogSink> LogService for Forwarding<'_, S> {
    fn write(
        &mut self,
        tag: ipc::wire::Str<'_, { LOG_TAG_MAX }>,
        message: ipc::wire::Str<'_, { LOG_MESSAGE_MAX }>,
    ) {
        let source = compose_source(self.source, tag.as_str());
        self.forwarded = Some(forward_message(
            self.sink,
            source.as_str(),
            message.as_str(),
        ));
    }
}

fn source_of(names: SourceNames, badge: u64) -> LogBuffer<LOG_SOURCE_MAX> {
    let mut source = LogBuffer::new();
    match names(badge) {
        Some(name) => {
            append_escaped(&mut source, name, false);
        }
        None => {
            let _ = write!(source, "badge-{badge:016x}");
        }
    }
    source
}

fn compose_source(trusted: &str, local: &str) -> LogBuffer<LOG_SOURCE_MAX> {
    let mut source = LogBuffer::new();
    source.push_str(trusted);
    if !local.is_empty() && source.as_str().len() < LOG_SOURCE_MAX {
        source.push_str("/");
        append_escaped(&mut source, local, true);
    }
    source
}

fn forward_message<S: LogSink>(sink: &mut S, source: &str, message: &str) -> Result<(), IpcError> {
    if message.is_empty() {
        return sink.write(source, "");
    }

    let mut chunk = LogBuffer::<LOG_MESSAGE_MAX>::new();
    // Экранирование растёт по одному scalar, поэтому кусок сбрасывается в сток только
    // между целыми scalar.
    for character in message.chars() {
        let escaped = escape_character(character, false);
        if escaped.as_str().len() > LOG_MESSAGE_MAX - chunk.as_str().len() {
            sink.write(source, chunk.as_str())?;
            chunk.clear();
        }
        chunk.push_str(escaped.as_str());
    }
    sink.write(source, chunk.as_str())
}

fn append_escaped<const N: usize>(buffer: &mut LogBuffer<N>, text: &str, brackets: bool) {
    for character in text.chars() {
        let escaped = escape_character(character, brackets);
        if escaped.as_str().len() > N - buffer.as_str().len() {
            break;
        }
        buffer.push_str(escaped.as_str());
    }
}

fn escape_character(character: char, brackets: bool) -> LogBuffer<ESCAPED_CHARACTER_MAX> {
    let mut escaped = LogBuffer::new();
    match character {
        '\n' => {
            escaped.push_str("\\n");
        }
        '\r' => {
            escaped.push_str("\\r");
        }
        '\t' => {
            escaped.push_str("\\t");
        }
        '[' | ']' if brackets => {
            escaped.push_str("\\");
            let mut bytes = [0; 4];
            escaped.push_str(character.encode_utf8(&mut bytes));
        }
        // Unicode line separators создают физическую строку, а bidi-controls
        // могут визуально переставить доверенный prefix при отображении журнала.
        '\u{061c}'
        | '\u{200e}'..='\u{200f}'
        | '\u{2028}'..='\u{202e}'
        | '\u{2066}'..='\u{2069}' => {
            let _ = write!(escaped, "\\u{{{:x}}}", u32::from(character));
        }
        control if control.is_control() => {
            let _ = write!(escaped, "\\u{{{:x}}}", u32::from(control));
        }
        printable => {
            let mut bytes = [0; 4];
            escaped.push_str(printable.encode_utf8(&mut bytes));
        }
    }
    escaped
}

fn discard_staged_caps(transport: &PortTransport) {
    // Capability, приехавшие с кадром, ядро уже положило в таблицу relay. Контракт их
    // не несёт, и дальше они не идут.
    for handle in transport.staged_caps().into_iter().flatten() {
        let _ = handle_close(handle);
    }
}
