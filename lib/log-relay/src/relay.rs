//! Пересылка строк лога доверенному стоку: источник строки выводится из значка
//! сессии, capability из принятых кадров не переносятся дальше.

use core::fmt::Write as _;

use ipc::{Transport as _, wire::IpcError};
use kernel_objects::{OwnedHandle, ReceivedFrame};
use log_abi::{LOG_MESSAGE_MAX, LOG_SOURCE_MAX, LOG_TAG_MAX, LogBuffer, log};
use runtime::PortTransport;
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

/// Имена источников строк журнала: значок лог-сессии - имя его владельца. Имена принадлежат
/// реализации, а relay уносит копию: его поток переживает смену состава композиции.
pub trait SourceNames: Send {
    /// Имя владельца значка `badge`. Отсутствие имени уводит значок в журнал числовой формой.
    fn name(&self, badge: u64) -> Option<LogBuffer<LOG_SOURCE_MAX>>;
}

pub(crate) struct LogRelay<S: LogSink, N: SourceNames> {
    receiver: OwnedHandle,
    names: N,
    sink: S,
}

impl<S: LogSink, N: SourceNames> LogRelay<S, N> {
    pub(crate) fn new(receiver: OwnedHandle, names: N, sink: S) -> Self {
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

            // Приём выше доставил кадр. Capability контракт не передаёт, и владеющий кадр
            // закрывает их вместе с незавершённым ответом.
            let Some(received) = ReceivedFrame::take(&transport) else {
                continue;
            };
            let source = source_of(&self.names, received.badge());

            let mut forwarding = Forwarding {
                sink: &mut self.sink,
                source: source.as_str(),
                forwarded: None,
            };
            let _ = log::dispatch(&mut forwarding, &transport);
            let forwarded = forwarding.forwarded;
            drop(received);

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

impl<S: LogSink> log::Service for Forwarding<'_, S> {
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

fn source_of<N: SourceNames>(names: &N, badge: u64) -> LogBuffer<LOG_SOURCE_MAX> {
    let mut source = LogBuffer::new();
    match names.name(badge) {
        Some(name) => {
            append_escaped(&mut source, name.as_str(), false);
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
