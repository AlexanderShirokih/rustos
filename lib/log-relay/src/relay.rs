//! Пересылка строк лога доверенному стоку: источник строки выводится из значка
//! сессии, capability из принятых кадров не переносятся дальше.

use core::fmt::Write as _;

use ipc::{Transport as _, wire::IpcError};
use log_abi::{LOG_MESSAGE_MAX, LOG_SOURCE_MAX, LOG_TAG_MAX, LogBuffer, LogService, dispatch_log};
use runtime::{OwnedHandle, PortTransport, handle_close};
use syscall::PORT_TIMEOUT_INFINITE;

// Источник собственных строк relay в стоке.
const RELAY_SOURCE: &str = "log-relay";

// Подряд идущих отказов стока, после которых пересылка прекращается.
const SINK_FAILURE_LIMIT: u32 = 8;

/// Сток строк лога: доверенный канал, недоступный клиентам лог-порта.
pub trait LogSink {
    /// Пишет строку источника `source` в журнал.
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
            match transport.wait_readable(PORT_TIMEOUT_INFINITE) {
                Ok(()) => {}
                // Уход значка кадра не несёт: состояния сессий relay не держит.
                Err(IpcError::SenderGone) => continue,
                // Бессрочный приём отказывает только исчерпанием получателей.
                Err(_) => return "stopped: log port closed",
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
        let mut source = LogBuffer::<LOG_SOURCE_MAX>::new();
        source.push_str(self.source);
        if !tag.as_str().is_empty() {
            source.push_str("/");
            source.push_str(tag.as_str());
        }
        self.forwarded = Some(self.sink.write(source.as_str(), message.as_str()));
    }
}

fn source_of(names: SourceNames, badge: u64) -> LogBuffer<LOG_SOURCE_MAX> {
    let mut source = LogBuffer::new();
    match names(badge) {
        Some(name) => {
            source.push_str(name);
        }
        None => {
            let _ = write!(source, "badge-{badge:016x}");
        }
    }
    source
}

// Capability, приехавшие с кадром, ядро уже положило в таблицу relay: контракт
// их не несёт, и дальше они не идут.
fn discard_staged_caps(transport: &PortTransport) {
    for handle in transport.staged_caps().into_iter().flatten() {
        let _ = handle_close(handle);
    }
}
